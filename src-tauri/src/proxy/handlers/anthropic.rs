use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::StreamExt;

use crate::proxy::handlers::common::{
    apply_retry_strategy, determine_retry_strategy, effective_max_retries, is_auth_error,
    rate_limit_duration_for_status, should_rotate_account,
};
use crate::proxy::handlers::protocol_convert::{
    anthropic_to_openai_request, openai_to_anthropic_response, StreamConverter,
};
use crate::proxy::middleware::monitor::UpstreamUrl;
use crate::proxy::server::AppState;

/// POST /v1/messages
///
/// Accepts an Anthropic-format request, converts it to OpenAI format,
/// forwards to the upstream New API site via `/v1/chat/completions`,
/// then converts the response back to Anthropic format.
pub async fn handle_messages(
    State(state): State<AppState>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (mut response, upstream_url) = handle_messages_inner(state, body).await;
    if let Some(url) = upstream_url {
        response.extensions_mut().insert(UpstreamUrl(url));
    }
    response
}

async fn handle_messages_inner(
    state: AppState,
    body: Bytes,
) -> (Response, Option<String>) {
    // Parse the Anthropic request body
    let anthropic_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                (
                    StatusCode::BAD_REQUEST,
                    error_json(&format!("Invalid JSON: {}", e)),
                )
                    .into_response(),
                None,
            );
        }
    };

    let is_stream = anthropic_body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    let model = anthropic_body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();

    // Resolve model alias
    let model = if !model.is_empty() {
        let resolved = state.model_router.resolve_alias(&model);
        if resolved != model {
            tracing::info!(original = %model, resolved = %resolved, "Anthropic: model alias resolved");
        }
        resolved
    } else {
        model
    };

    // Convert Anthropic request → OpenAI request
    let openai_body = anthropic_to_openai_request(&anthropic_body);
    let openai_bytes = Bytes::from(serde_json::to_vec(&openai_body).unwrap_or_default());

    // Use OpenAI protocol for upstream selection — all New API sites support /v1/chat/completions
    let mut failed_accounts: Vec<String> = Vec::new();
    let active_count = state.token_manager.active_healthy_count(Some("openai"));
    let max_retries = effective_max_retries(active_count);
    let mut last_site_url: Option<String> = None;

    tracing::info!(
        model = %model,
        is_stream,
        active_count,
        max_retries,
        "Anthropic→OpenAI: starting request"
    );

    for attempt in 0..=max_retries {
        let token = match state.token_manager.get_token_excluding(
            None,
            if model.is_empty() { None } else { Some(&model) },
            Some("openai"),
            &failed_accounts,
        ) {
            Some(t) => {
                tracing::debug!(
                    account_id = %t.account_id,
                    site_name = %t.site_name,
                    attempt,
                    "Anthropic→OpenAI: selected account"
                );
                t
            }
            None => {
                tracing::warn!(
                    attempt,
                    failed_count = failed_accounts.len(),
                    model = %model,
                    "Anthropic→OpenAI: no available accounts"
                );
                return (
                    anthropic_error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "overloaded_error",
                        "No available accounts",
                    ),
                    last_site_url,
                );
            }
        };

        last_site_url = Some(token.site_url.clone());

        let mut req_headers = HeaderMap::new();
        req_headers.insert(
            axum::http::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );

        let result = state
            .upstream
            .forward(
                &token.site_url,
                "/v1/chat/completions",
                reqwest::Method::POST,
                req_headers,
                openai_bytes.clone(),
                token.upstream_credential(),
            )
            .await;

        match result {
            Ok(resp) => {
                let status = resp.status().as_u16();

                if status >= 200 && status < 300 {
                    state.token_manager.mark_success(&token.account_id);

                    if is_stream {
                        return (convert_stream_response(resp, &model), last_site_url);
                    }
                    return (convert_non_stream_response(resp, &model).await, last_site_url);
                }

                // Non-2xx: save headers before consuming body
                let resp_headers = resp.headers().clone();
                let error_body = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "(failed to read body)".to_string());
                tracing::warn!(
                    account_id = %token.account_id,
                    site_name = %token.site_name,
                    status,
                    attempt,
                    upstream_error = %error_body,
                    "Anthropic→OpenAI: upstream returned error"
                );

                // 404 = model not found → mark account models as stale
                // and immediately remove the model from this account's registry
                // so future requests won't route to it for this model.
                if status == 404 {
                    crate::proxy::model_cache::global().mark_stale(&token.account_id);
                    if !model.is_empty() {
                        state.token_manager.remove_model_for_account(&token.account_id, &model);
                    }
                }

                if should_rotate_account(status) && attempt < max_retries {
                    if is_auth_error(status) {
                        state
                            .token_manager
                            .mark_auth_failed(&token.account_id, status);
                    } else {
                        let retry_after = resp_headers
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
                        return (
                            anthropic_error_response(
                                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                                "api_error",
                                &format!("Upstream returned {}", status),
                            ),
                            last_site_url,
                        );
                    }
                    tracing::warn!(
                        account_id = %token.account_id,
                        status,
                        attempt,
                        "Rotating account for Anthropic→OpenAI request"
                    );
                    continue;
                }

                // Non-retryable or max retries reached
                if is_auth_error(status) {
                    state
                        .token_manager
                        .mark_auth_failed(&token.account_id, status);
                    return (
                        anthropic_error_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "overloaded_error",
                            "All upstream accounts failed authentication",
                        ),
                        last_site_url,
                    );
                } else if status >= 500 {
                    state.token_manager.mark_failed(&token.account_id);
                }

                return (
                    anthropic_error_response(
                        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                        "api_error",
                        &format!("Upstream returned {}", status),
                    ),
                    last_site_url,
                );
            }
            Err(e) => {
                state
                    .token_manager
                    .mark_connection_failed(&token.account_id);
                failed_accounts.push(token.account_id.clone());
                tracing::error!(
                    account_id = %token.account_id,
                    error = %e,
                    attempt,
                    "Anthropic→OpenAI upstream request failed"
                );

                if attempt >= max_retries {
                    return (
                        anthropic_error_response(
                            StatusCode::BAD_GATEWAY,
                            "api_error",
                            &format!("Upstream error: {}", e),
                        ),
                        last_site_url,
                    );
                }
            }
        }
    }

    (
        anthropic_error_response(
            StatusCode::BAD_GATEWAY,
            "overloaded_error",
            "All retry attempts failed",
        ),
        last_site_url,
    )
}

/// Convert a successful non-streaming OpenAI response to Anthropic format.
async fn convert_non_stream_response(resp: reqwest::Response, model: &str) -> Response {
    let status = resp.status().as_u16();
    match resp.bytes().await {
        Ok(body) => {
            let openai_json: serde_json::Value =
                serde_json::from_slice(&body).unwrap_or_default();
            let anthropic_json = openai_to_anthropic_response(&openai_json, model);
            let anthropic_bytes = serde_json::to_vec(&anthropic_json).unwrap_or_default();

            Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .header("content-type", "application/json")
                .body(Body::from(anthropic_bytes))
                .unwrap_or_else(|_| {
                    (StatusCode::INTERNAL_SERVER_ERROR, "Response error").into_response()
                })
        }
        Err(e) => anthropic_error_response(
            StatusCode::BAD_GATEWAY,
            "api_error",
            &format!("Failed to read upstream body: {}", e),
        ),
    }
}

/// Convert a successful streaming OpenAI response to Anthropic SSE format.
fn convert_stream_response(resp: reqwest::Response, model: &str) -> Response {
    let model = model.to_string();
    let upstream_stream = resp.bytes_stream();

    let converted = async_stream::stream! {
        let mut converter = StreamConverter::new(model);
        let mut buffer = String::new();

        tokio::pin!(upstream_stream);

        while let Some(chunk_result) = upstream_stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let text = match std::str::from_utf8(&chunk) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };

                    buffer.push_str(text);

                    // Process complete SSE lines
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].trim_end_matches('\r').to_string();
                        buffer = buffer[pos + 1..].to_string();

                        if line.is_empty() {
                            continue;
                        }

                        // Extract the data from "data: ..." lines
                        if let Some(data) = line.strip_prefix("data: ") {
                            let events = converter.process_chunk(data);
                            for event in events {
                                yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("{}\n", event)));
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Error reading upstream stream");
                    break;
                }
            }
        }

        // Process any remaining data in buffer
        if !buffer.trim().is_empty() {
            for line in buffer.lines() {
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data: ") {
                    let events = converter.process_chunk(data);
                    for event in events {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("{}\n", event)));
                    }
                }
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(converted))
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "Stream error").into_response()
        })
}

/// Build an Anthropic-format error response.
fn anthropic_error_response(status: StatusCode, error_type: &str, message: &str) -> Response {
    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": error_type,
            "message": message
        }
    });
    (status, axum::Json(body)).into_response()
}

fn error_json(message: &str) -> String {
    serde_json::json!({
        "type": "error",
        "error": {
            "type": "invalid_request_error",
            "message": message
        }
    })
    .to_string()
}
