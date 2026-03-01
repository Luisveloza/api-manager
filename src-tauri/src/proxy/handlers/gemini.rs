use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;

use crate::proxy::handlers::common::{
    apply_retry_strategy, determine_retry_strategy, effective_max_retries, is_auth_error,
    rate_limit_duration_for_status, should_rotate_account,
};
use crate::proxy::middleware::monitor::UpstreamUrl;
use crate::proxy::server::AppState;

/// POST /v1beta/models/:model_action
/// e.g. /v1beta/models/gemini-pro:generateContent
pub async fn handle_generate(
    State(state): State<AppState>,
    Path(model_action): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = format!("/v1beta/models/{}", model_action);
    // Extract model name from "gemini-pro:generateContent" → "gemini-pro"
    let model = model_action.split(':').next().map(String::from);
    let mut failed_accounts: Vec<String> = Vec::new();
    let max_retries = effective_max_retries(
        state.token_manager.active_healthy_count(Some("gemini")),
    );
    let mut last_site_url: Option<String> = None;

    for attempt in 0..=max_retries {
        let token = match state.token_manager.get_token_excluding(
            None,
            model.as_deref(),
            Some("gemini"),
            &failed_accounts,
        ) {
            Some(t) => t,
            None => {
                let mut resp = (
                    StatusCode::SERVICE_UNAVAILABLE,
                    error_json("No available accounts"),
                )
                    .into_response();
                if let Some(url) = last_site_url {
                    resp.extensions_mut().insert(UpstreamUrl(url));
                }
                return resp;
            }
        };

        last_site_url = Some(token.site_url.clone());

        let result = state
            .upstream
            .forward(
                &token.site_url,
                &path,
                reqwest::Method::POST,
                headers.clone(),
                body.clone(),
                token.upstream_credential(),
            )
            .await;

        match result {
            Ok(resp) => {
                let status = resp.status().as_u16();

                if status >= 200 && status < 300 {
                    state.token_manager.mark_success(&token.account_id);
                    let mut response = convert_reqwest_response(resp).await;
                    response.extensions_mut().insert(UpstreamUrl(token.site_url.clone()));
                    return response;
                }

                // 404 = model not found → mark account models as stale
                // and immediately remove the model from this account's registry
                // so future requests won't route to it for this model.
                if status == 404 {
                    crate::proxy::model_cache::global().mark_stale(&token.account_id);
                    if let Some(ref m) = model {
                        state.token_manager.remove_model_for_account(&token.account_id, m);
                    }
                }

                if should_rotate_account(status) && attempt < max_retries {
                    // Auth errors: immediately disable the account, don't just rate-limit
                    if is_auth_error(status) {
                        state
                            .token_manager
                            .mark_auth_failed(&token.account_id, status);
                    } else {
                        let retry_after = resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.parse::<u64>().ok())
                            .map(std::time::Duration::from_secs);

                        let cooldown = rate_limit_duration_for_status(status, retry_after);
                        state
                            .token_manager
                            .mark_rate_limited(&token.account_id, status, Some(cooldown));
                    }

                    failed_accounts.push(token.account_id.clone());
                    let strategy = determine_retry_strategy(status, "");
                    if !apply_retry_strategy(&strategy, attempt).await {
                        let mut response = convert_reqwest_response(resp).await;
                        response.extensions_mut().insert(UpstreamUrl(token.site_url.clone()));
                        return response;
                    }
                    continue;
                }

                if is_auth_error(status) {
                    state
                        .token_manager
                        .mark_auth_failed(&token.account_id, status);
                    let mut resp = (
                        StatusCode::SERVICE_UNAVAILABLE,
                        error_json("All upstream accounts failed authentication"),
                    )
                        .into_response();
                    resp.extensions_mut().insert(UpstreamUrl(token.site_url.clone()));
                    return resp;
                } else if status >= 500 {
                    state.token_manager.mark_failed(&token.account_id);
                }
                let mut response = convert_reqwest_response(resp).await;
                response.extensions_mut().insert(UpstreamUrl(token.site_url.clone()));
                return response;
            }
            Err(e) => {
                // Connection-level failure (timeout, DNS, TCP refused, etc.)
                state
                    .token_manager
                    .mark_connection_failed(&token.account_id);
                failed_accounts.push(token.account_id.clone());
                if attempt >= max_retries {
                    let mut resp = (
                        StatusCode::BAD_GATEWAY,
                        error_json(&format!("Upstream error: {}", e)),
                    )
                        .into_response();
                    resp.extensions_mut().insert(UpstreamUrl(token.site_url.clone()));
                    return resp;
                }
            }
        }
    }

    let mut resp = (
        StatusCode::BAD_GATEWAY,
        error_json("All retry attempts failed"),
    )
        .into_response();
    if let Some(url) = last_site_url {
        resp.extensions_mut().insert(UpstreamUrl(url));
    }
    resp
}

/// GET /v1beta/models
pub async fn handle_list_models(State(state): State<AppState>) -> impl IntoResponse {
    let token = match state.token_manager.get_token(None, None, Some("gemini")) {
        Some(t) => t,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                error_json("No available accounts"),
            )
                .into_response()
        }
    };

    match state
        .upstream
        .forward(
            &token.site_url,
            "/v1beta/models",
            reqwest::Method::GET,
            HeaderMap::new(),
            Bytes::new(),
            &token.access_token,
        )
        .await
    {
        Ok(resp) => convert_reqwest_response(resp).await,
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            error_json(&format!("Upstream error: {}", e)),
        )
            .into_response(),
    }
}

async fn convert_reqwest_response(resp: reqwest::Response) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
    let headers = resp.headers().clone();
    match resp.bytes().await {
        Ok(body) => {
            let mut builder = Response::builder().status(status);
            for (key, value) in headers.iter() {
                builder = builder.header(key.clone(), value.clone());
            }
            builder.body(Body::from(body)).unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "Response error").into_response()
            })
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            error_json(&format!("Failed to read upstream body: {}", e)),
        )
            .into_response(),
    }
}

fn error_json(message: &str) -> String {
    serde_json::json!({
        "error": {
            "message": message,
            "type": "proxy_error",
            "code": "proxy_error"
        }
    })
    .to_string()
}
