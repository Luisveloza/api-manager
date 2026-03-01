use axum::{
    body::Body,
    extract::State,
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

/// POST /v1/chat/completions
pub async fn handle_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward_with_retry(state, "/v1/chat/completions", headers, body).await
}

/// POST /v1/completions
pub async fn handle_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward_with_retry(state, "/v1/completions", headers, body).await
}

/// GET /v1/models — returns aggregated model list from all accounts.
pub async fn handle_list_models(State(state): State<AppState>) -> impl IntoResponse {
    let models = state.token_manager.get_all_models().await;

    if models.is_empty() {
        // Fallback: try fetching from a single upstream (original behavior)
        let token = match state.token_manager.get_token(None, None, Some("openai")) {
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
                "/v1/models",
                reqwest::Method::GET,
                HeaderMap::new(),
                Bytes::new(),
                token.upstream_credential(),
            )
            .await
        {
            Ok(resp) => return convert_reqwest_response(resp).await,
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    error_json(&format!("Upstream error: {}", e)),
                )
                    .into_response()
            }
        }
    }

    // Build OpenAI-compatible /v1/models response
    let data: Vec<serde_json::Value> = models
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "object": "model",
                "owned_by": "system",
            })
        })
        .collect();

    let response = serde_json::json!({
        "object": "list",
        "data": data,
    });

    (StatusCode::OK, axum::Json(response)).into_response()
}

pub async fn forward_with_retry(
    state: AppState,
    path: &str,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (mut response, upstream_url) = forward_with_retry_inner(state, path, headers, body).await;
    if let Some(url) = upstream_url {
        response.extensions_mut().insert(UpstreamUrl(url));
    }
    response
}

async fn forward_with_retry_inner(
    state: AppState,
    path: &str,
    headers: HeaderMap,
    body: Bytes,
) -> (Response, Option<String>) {
    let is_stream = extract_stream_flag(&body);
    let original_model = extract_model(&body);

    // Resolve model alias
    let (model, body) = if let Some(ref m) = original_model {
        let resolved = state.model_router.resolve_alias(m);
        if resolved != *m {
            tracing::info!(original = %m, resolved = %resolved, "Model alias resolved");
            // Replace model in body
            let new_body = replace_model_in_body(&body, &resolved);
            (Some(resolved), new_body)
        } else {
            (original_model, body)
        }
    } else {
        (original_model, body)
    };

    let mut failed_accounts: Vec<String> = Vec::new();
    let max_retries = effective_max_retries(
        state.token_manager.active_healthy_count(Some("openai")),
    );
    let mut last_site_url: Option<String> = None;

    for attempt in 0..=max_retries {
        let token = match state.token_manager.get_token_excluding(
            None,
            model.as_deref(),
            Some("openai"),
            &failed_accounts,
        ) {
            Some(t) => t,
            None => {
                return (
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        error_json("No available accounts"),
                    )
                        .into_response(),
                    last_site_url,
                );
            }
        };

        last_site_url = Some(token.site_url.clone());

        let result = state
            .upstream
            .forward(
                &token.site_url,
                path,
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

                    if is_stream {
                        return (stream_response(resp), last_site_url);
                    }
                    return (convert_reqwest_response(resp).await, last_site_url);
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
                        return (convert_reqwest_response(resp).await, last_site_url);
                    }
                    tracing::warn!(
                        account_id = %token.account_id,
                        status,
                        attempt,
                        "Rotating account due to upstream error"
                    );
                    continue;
                }

                // Non-retryable error or max retries reached
                if is_auth_error(status) {
                    state
                        .token_manager
                        .mark_auth_failed(&token.account_id, status);
                    return (
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            error_json("All upstream accounts failed authentication"),
                        )
                            .into_response(),
                        last_site_url,
                    );
                } else if status >= 500 {
                    state.token_manager.mark_failed(&token.account_id);
                }
                return (convert_reqwest_response(resp).await, last_site_url);
            }
            Err(e) => {
                // Connection-level failure (timeout, DNS, TCP refused, etc.)
                state
                    .token_manager
                    .mark_connection_failed(&token.account_id);
                failed_accounts.push(token.account_id.clone());
                tracing::error!(
                    account_id = %token.account_id,
                    error = %e,
                    attempt,
                    "Upstream request failed"
                );

                if attempt >= max_retries {
                    return (
                        (
                            StatusCode::BAD_GATEWAY,
                            error_json(&format!("Upstream error: {}", e)),
                        )
                            .into_response(),
                        last_site_url,
                    );
                }
            }
        }
    }

    (
        (
            StatusCode::BAD_GATEWAY,
            error_json("All retry attempts failed"),
        )
            .into_response(),
        last_site_url,
    )
}

fn extract_stream_flag(body: &Bytes) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false)
}

fn extract_model(body: &Bytes) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(String::from))
}

fn replace_model_in_body(body: &Bytes, new_model: &str) -> Bytes {
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "model".to_string(),
                    serde_json::Value::String(new_model.to_string()),
                );
            }
            Bytes::from(serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec()))
        }
        Err(_) => body.clone(),
    }
}

fn stream_response(resp: reqwest::Response) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);

    let mut builder = Response::builder().status(status);

    // Copy headers from upstream response
    for (key, value) in resp.headers() {
        builder = builder.header(key.clone(), value.clone());
    }

    let stream = resp.bytes_stream();
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "Stream error")
                .into_response()
        })
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
            builder
                .body(Body::from(body))
                .unwrap_or_else(|_| {
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
