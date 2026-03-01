use axum::{
    body::Body,
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use bytes::Bytes;
use http_body_util::BodyExt;
use std::sync::Arc;
use std::time::Instant;

use crate::proxy::middleware::auth::AuthenticatedKey;
use crate::proxy::monitor::{ProxyMonitor, ProxyRequestLog};

/// Extension type set by handlers to pass the upstream URL to the monitor middleware.
#[derive(Clone)]
pub struct UpstreamUrl(pub String);

/// Max bytes of request/response body to store per log entry.
const BODY_TRUNCATE_BYTES: usize = 4096;

/// Middleware that records request/response metadata into ProxyMonitor.
pub async fn monitor_middleware(
    State(monitor): State<Arc<ProxyMonitor>>,
    request: Request,
    next: Next,
) -> Response {
    if !monitor.is_enabled() {
        return next.run(request).await;
    }

    let method = request.method().to_string();
    let url = request.uri().path().to_string();
    let client_ip = extract_client_ip(&request);

    // Extract model and a truncated copy of the request body
    let (model, req_body_snapshot, request) = extract_model_from_request(request).await;

    // Grab authenticated key info (if present) before running downstream
    let auth_key = request.extensions().get::<AuthenticatedKey>().cloned();

    let start = Instant::now();
    let response = next.run(request).await;
    let duration_ms = start.elapsed().as_millis() as u64;

    let status = response.status().as_u16();

    // Extract token usage and a truncated copy of the response body
    let (input_tokens, output_tokens, error_msg, resp_body_snapshot, response) =
        extract_usage_from_response(response).await;

    let log = ProxyRequestLog {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        method,
        url,
        status,
        duration_ms,
        model: model.clone(),
        account_id: None, // filled by handler layer if needed
        upstream_url: response.extensions().get::<UpstreamUrl>().map(|u| u.0.clone()),
        client_ip,
        input_tokens,
        output_tokens,
        error: error_msg,
        estimated_cost: estimate_cost(&model, input_tokens, output_tokens),
        request_body: req_body_snapshot,
        response_body: resp_body_snapshot,
        api_key: auth_key.map(|k| k.key),
    };

    // Record stats
    crate::proxy::proxy_stats::global().record(&log);

    monitor.add_log(log);

    response
}

fn extract_client_ip(request: &Request) -> Option<String> {
    // Try X-Forwarded-For first, then X-Real-Ip
    request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').next().unwrap_or("").trim().to_string())
        .or_else(|| {
            request
                .headers()
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
}

async fn extract_model_from_request(request: Request) -> (Option<String>, Option<String>, Request) {
    let (parts, body) = request.into_parts();

    // Only attempt to parse JSON bodies
    let content_type = parts
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !content_type.contains("application/json") {
        return (None, None, Request::from_parts(parts, body));
    }

    // Collect body bytes (limit to 10MB for safety)
    match body.collect().await {
        Ok(collected) => {
            let bytes = collected.to_bytes();

            let model = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(String::from));

            let snapshot = truncate_body(&bytes);

            let new_body = Body::from(bytes);
            (model, Some(snapshot), Request::from_parts(parts, new_body))
        }
        Err(_) => (None, None, Request::from_parts(parts, Body::empty())),
    }
}

async fn extract_usage_from_response(
    response: Response,
) -> (Option<i32>, Option<i32>, Option<String>, Option<String>, Response) {
    let (parts, body) = response.into_parts();

    let content_type = parts
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Only parse JSON responses for usage data
    if !content_type.contains("application/json") {
        return (
            None,
            None,
            None,
            None,
            Response::from_parts(parts, body),
        );
    }

    match body.collect().await {
        Ok(collected) => {
            let bytes = collected.to_bytes();

            let (input_tokens, output_tokens, error_msg) =
                parse_usage_and_error(&bytes);

            let snapshot = truncate_body(&bytes);

            let new_body = Body::from(bytes);
            (
                input_tokens,
                output_tokens,
                error_msg,
                Some(snapshot),
                Response::from_parts(parts, new_body),
            )
        }
        Err(_) => (None, None, None, None, Response::from_parts(parts, Body::empty())),
    }
}

fn parse_usage_and_error(bytes: &Bytes) -> (Option<i32>, Option<i32>, Option<String>) {
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return (None, None, None),
    };

    let input_tokens = value
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);

    let output_tokens = value
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);

    let error_msg = value
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(String::from);

    (input_tokens, output_tokens, error_msg)
}

/// Estimate cost using the global price cache.
fn estimate_cost(
    model: &Option<String>,
    input_tokens: Option<i32>,
    output_tokens: Option<i32>,
) -> Option<f64> {
    let model_name = model.as_deref()?;
    let input = input_tokens.unwrap_or(0);
    let output = output_tokens.unwrap_or(0);
    if input == 0 && output == 0 {
        return None;
    }
    crate::proxy::price_cache::global().estimate_cost(model_name, input, output)
}

/// Truncate a body to `BODY_TRUNCATE_BYTES` and return a UTF-8 string.
/// Non-UTF-8 bytes are replaced with the Unicode replacement character.
fn truncate_body(bytes: &Bytes) -> String {
    let slice = if bytes.len() > BODY_TRUNCATE_BYTES {
        &bytes[..BODY_TRUNCATE_BYTES]
    } else {
        bytes.as_ref()
    };
    String::from_utf8_lossy(slice).into_owned()
}
