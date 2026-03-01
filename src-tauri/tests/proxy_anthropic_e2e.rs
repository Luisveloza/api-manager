//! End-to-end integration tests for the Anthropic proxy handler.
//!
//! Spins up a **fake upstream** (mock New API) and a **real proxy server**,
//! then hits the proxy with Anthropic-format requests and verifies the full
//! request→convert→forward→convert→response chain.

use axum::{
    body::Body,
    extract::State as AxState,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Fake upstream (mock New API /v1/chat/completions & /v1/models)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FakeUpstreamState {
    /// Every request body received by the fake upstream.
    captured_requests: Arc<Mutex<Vec<Value>>>,
    /// How many requests have been received.
    request_count: Arc<AtomicUsize>,
    /// If set, the fake upstream will return this status code instead of 200.
    force_status: Arc<Mutex<Option<u16>>>,
    /// Whether to return SSE stream or JSON (reserved for future tests).
    #[allow(dead_code)]
    stream_mode: Arc<Mutex<bool>>,
}

impl FakeUpstreamState {
    fn new() -> Self {
        Self {
            captured_requests: Arc::new(Mutex::new(Vec::new())),
            request_count: Arc::new(AtomicUsize::new(0)),
            force_status: Arc::new(Mutex::new(None)),
            stream_mode: Arc::new(Mutex::new(false)),
        }
    }
}

async fn fake_chat_completions(
    AxState(state): AxState<FakeUpstreamState>,
    body: Bytes,
) -> impl IntoResponse {
    state.request_count.fetch_add(1, Ordering::Relaxed);

    let body_json: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    state.captured_requests.lock().await.push(body_json.clone());

    // Check if we should force an error
    if let Some(status) = *state.force_status.lock().await {
        return (
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(json!({
                "error": {"message": "forced error", "type": "test_error", "code": ""}
            })),
        )
            .into_response();
    }

    let is_stream = body_json
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    let model = body_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown");

    if is_stream {
        // Return SSE stream
        let model = model.to_string();
        let stream = async_stream::stream! {
            // Chunk 1: role
            yield Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                "data: {}\n\n",
                json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion.chunk",
                    "model": model,
                    "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 0}
                })
            )));

            // Chunk 2: content
            yield Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                "data: {}\n\n",
                json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion.chunk",
                    "model": model,
                    "choices": [{"index": 0, "delta": {"content": "Hello from mock!"}, "finish_reason": null}]
                })
            )));

            // Chunk 3: finish
            yield Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                "data: {}\n\n",
                json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion.chunk",
                    "model": model,
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5}
                })
            )));

            // [DONE]
            yield Ok::<Bytes, std::io::Error>(Bytes::from("data: [DONE]\n\n"));
        };

        return axum::response::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap()
            .into_response();
    }

    // Non-streaming JSON response
    (
        StatusCode::OK,
        Json(json!({
            "id": "chatcmpl-test-123",
            "object": "chat.completion",
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello from mock upstream!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        })),
    )
        .into_response()
}

async fn fake_list_models() -> impl IntoResponse {
    Json(json!({
        "data": [
            {"id": "claude-sonnet-4-20250514"},
            {"id": "gpt-4o"},
        ]
    }))
}

/// Start a fake upstream on a random port, return (addr, state).
async fn start_fake_upstream() -> (SocketAddr, FakeUpstreamState) {
    let state = FakeUpstreamState::new();

    let app = Router::new()
        .route("/v1/chat/completions", post(fake_chat_completions))
        .route("/v1/models", get(fake_list_models))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Wait for listener to be ready
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (addr, state)
}

// ---------------------------------------------------------------------------
// Helpers to create proxy config + fake account
// ---------------------------------------------------------------------------

fn make_test_account(id: &str, site_url: &str) -> apimanager_lib::models::SiteAccount {
    apimanager_lib::models::SiteAccount {
        id: id.to_string(),
        site_name: format!("test-{}", id),
        site_url: site_url.to_string(),
        site_type: "new-api".to_string(),
        account_info: apimanager_lib::models::AccountInfo {
            id: 1,
            access_token: "test-access-token".to_string(),
            api_key: Some("sk-test-key-12345".to_string()),
            username: "tester".to_string(),
            quota: 10000.0,
            today_prompt_tokens: 0,
            today_completion_tokens: 0,
            today_quota_consumption: 0.0,
            today_requests_count: 0,
            today_income: 0.0,
        },
        auth_type: "access_token".to_string(),
        last_sync_time: 0,
        updated_at: 0,
        created_at: 0,
        notes: None,
        disabled: Some(false),
        health: None,
        exchange_rate: None,
        proxy_health: None,
        proxy_priority: 0,
        proxy_weight: 10,
    }
}

fn make_test_proxy_config(port: u16) -> apimanager_lib::models::ProxyConfig {
    apimanager_lib::models::ProxyConfig {
        enabled: true,
        port,
        api_key: String::new(), // no auth for tests
        admin_password: None,
        auth_mode: apimanager_lib::models::ProxyAuthMode::Off,
        allow_lan_access: false,
        auto_start: false,
        request_timeout: 30,
        enable_logging: true,
        upstream_proxy: apimanager_lib::models::UpstreamProxyConfig::default(),
        load_balance_mode: apimanager_lib::models::LoadBalanceMode::default(),
        daily_cost_limit: 0.0,
        monthly_cost_limit: 0.0,
        budget_exceeded_action: "warn".to_string(),
        model_aliases: vec![],
        model_routes: vec![],
        api_keys: vec![],
    }
}

/// Find a free TCP port.
async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

// ===========================================================================
// Tests
// ===========================================================================

/// Non-streaming Anthropic request → proxy converts → upstream returns JSON → proxy converts back.
#[tokio::test]
async fn anthropic_non_stream_round_trip() {
    let (upstream_addr, fake_state) = start_fake_upstream().await;
    let upstream_url = format!("http://{}", upstream_addr);

    let proxy_port = free_port().await;
    let config = make_test_proxy_config(proxy_port);
    let accounts = vec![make_test_account("acc-1", &upstream_url)];

    let mut server = apimanager_lib::proxy::server::start_server(&config, &accounts)
        .await
        .expect("proxy should start");

    // Wait for server to be ready
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Send Anthropic-format request
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/v1/messages", proxy_port))
        .header("content-type", "application/json")
        .json(&json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        }))
        .send()
        .await
        .expect("request should succeed");

    let status = resp.status().as_u16();
    let body: Value = resp.json().await.expect("should be valid JSON");

    // Verify: proxy returned 200
    assert_eq!(status, 200, "Expected 200, got {} — body: {}", status, body);

    // Verify: response is in Anthropic format
    assert_eq!(body["type"], "message", "Should be Anthropic message format");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["model"], "claude-sonnet-4-20250514");
    assert_eq!(body["stop_reason"], "end_turn");

    let content = body["content"]
        .as_array()
        .expect("content should be array");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "Hello from mock upstream!");

    // Verify: usage is present
    assert!(body["usage"]["input_tokens"].as_u64().unwrap() > 0);
    assert!(body["usage"]["output_tokens"].as_u64().unwrap() > 0);

    // Verify: the request that hit the upstream was in OpenAI format
    let captured = fake_state.captured_requests.lock().await;
    assert_eq!(captured.len(), 1, "Upstream should receive exactly 1 request");
    let upstream_req = &captured[0];

    // Must have model
    assert_eq!(
        upstream_req["model"], "claude-sonnet-4-20250514",
        "OpenAI request must have model field"
    );
    // Must have messages array
    assert!(
        upstream_req["messages"].is_array(),
        "OpenAI request must have messages array"
    );
    // Must have max_tokens
    assert_eq!(upstream_req["max_tokens"], 1024);
    // Must NOT have stream=true (not requested)
    assert!(
        upstream_req.get("stream").is_none()
            || upstream_req["stream"] == false
            || upstream_req["stream"].is_null(),
        "Non-stream request should not have stream=true"
    );

    server.stop().await;
}

/// Anthropic request with system prompt → system becomes a system message in OpenAI format.
#[tokio::test]
async fn anthropic_system_prompt_conversion() {
    let (upstream_addr, fake_state) = start_fake_upstream().await;
    let upstream_url = format!("http://{}", upstream_addr);

    let proxy_port = free_port().await;
    let config = make_test_proxy_config(proxy_port);
    let accounts = vec![make_test_account("acc-sys", &upstream_url)];

    let mut server = apimanager_lib::proxy::server::start_server(&config, &accounts)
        .await
        .expect("proxy should start");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/v1/messages", proxy_port))
        .header("content-type", "application/json")
        .json(&json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 512,
            "system": "You are a helpful assistant.",
            "messages": [
                {"role": "user", "content": "Hi"}
            ]
        }))
        .send()
        .await
        .expect("request should succeed");

    assert_eq!(resp.status().as_u16(), 200);

    // Check upstream received system as first message
    let captured = fake_state.captured_requests.lock().await;
    let upstream_req = &captured[0];
    let messages = upstream_req["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "Should have system + user messages");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "You are a helpful assistant.");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "Hi");

    server.stop().await;
}

/// Streaming Anthropic request → proxy converts SSE format.
#[tokio::test]
async fn anthropic_stream_round_trip() {
    let (upstream_addr, _fake_state) = start_fake_upstream().await;
    let upstream_url = format!("http://{}", upstream_addr);

    let proxy_port = free_port().await;
    let config = make_test_proxy_config(proxy_port);
    let accounts = vec![make_test_account("acc-stream", &upstream_url)];

    let mut server = apimanager_lib::proxy::server::start_server(&config, &accounts)
        .await
        .expect("proxy should start");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/v1/messages", proxy_port))
        .header("content-type", "application/json")
        .json(&json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1024,
            "stream": true,
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        }))
        .send()
        .await
        .expect("request should succeed");

    assert_eq!(resp.status().as_u16(), 200);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/event-stream"),
        "Stream response should be text/event-stream, got: {}",
        content_type
    );

    // Read the full SSE body
    let body_text = resp.text().await.expect("should read body");

    // Should contain Anthropic SSE events
    assert!(
        body_text.contains("message_start"),
        "Should contain message_start event. Body: {}",
        &body_text[..body_text.len().min(500)]
    );
    assert!(
        body_text.contains("content_block_delta"),
        "Should contain content_block_delta event"
    );
    assert!(
        body_text.contains("Hello from mock!"),
        "Should contain the mock content"
    );
    assert!(
        body_text.contains("message_stop"),
        "Should contain message_stop event"
    );

    server.stop().await;
}

/// Account with no api_key should be filtered out, leaving no accounts → 503.
#[tokio::test]
async fn api_key_empty_returns_503() {
    let (upstream_addr, _) = start_fake_upstream().await;
    let upstream_url = format!("http://{}", upstream_addr);

    let proxy_port = free_port().await;
    let config = make_test_proxy_config(proxy_port);

    // Account with no api_key
    let mut account = make_test_account("acc-nokey", &upstream_url);
    account.account_info.api_key = None;

    let mut server = apimanager_lib::proxy::server::start_server(&config, &[account])
        .await
        .expect("proxy should start");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/v1/messages", proxy_port))
        .header("content-type", "application/json")
        .json(&json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .send()
        .await
        .expect("request should succeed");

    assert_eq!(
        resp.status().as_u16(),
        503,
        "No api_key → no accounts → 503"
    );

    server.stop().await;
}

/// Upstream returns 500 → proxy retries with a different account.
#[tokio::test]
async fn anthropic_retries_on_500() {
    let (upstream_addr, fake_state) = start_fake_upstream().await;
    let upstream_url = format!("http://{}", upstream_addr);

    let proxy_port = free_port().await;
    let config = make_test_proxy_config(proxy_port);
    let accounts = vec![
        make_test_account("acc-a", &upstream_url),
        make_test_account("acc-b", &upstream_url),
    ];

    let mut server = apimanager_lib::proxy::server::start_server(&config, &accounts)
        .await
        .expect("proxy should start");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // First request: force 500, then clear so retry succeeds
    {
        let mut fs = fake_state.force_status.lock().await;
        *fs = Some(500);
    }

    // Spawn a task to clear the forced error after the first request hits
    let fake_clone = fake_state.clone();
    tokio::spawn(async move {
        // Wait for the first request to be captured
        loop {
            if fake_clone.request_count.load(Ordering::Relaxed) >= 1 {
                // Clear after first failure so retry succeeds
                let mut fs = fake_clone.force_status.lock().await;
                *fs = None;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/v1/messages", proxy_port))
        .header("content-type", "application/json")
        .json(&json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .send()
        .await
        .expect("request should succeed");

    // Should eventually succeed after retry
    let status = resp.status().as_u16();
    // The retry might hit the same fake upstream (which is now returning 200)
    // so we should get 200 on the second attempt.
    assert_eq!(status, 200, "Should succeed after retry");

    // Upstream should have received >= 2 requests (first 500, then retry 200)
    let count = fake_state.request_count.load(Ordering::Relaxed);
    assert!(count >= 2, "Should have retried; got {} requests", count);

    server.stop().await;
}

/// Content-Type header must be present in the upstream request.
#[tokio::test]
async fn upstream_receives_content_type_json() {
    // This test uses a lower-level mock that captures raw headers.
    let captured_headers: Arc<Mutex<Option<axum::http::HeaderMap>>> =
        Arc::new(Mutex::new(None));

    let headers_clone = captured_headers.clone();
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                move |headers: axum::http::HeaderMap, body: Bytes| {
                    let headers_clone = headers_clone.clone();
                    async move {
                        *headers_clone.lock().await = Some(headers);
                        let body_json: Value =
                            serde_json::from_slice(&body).unwrap_or(json!({}));
                        let model = body_json
                            .get("model")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown");
                        Json(json!({
                            "id": "test",
                            "choices": [{
                                "message": {"role": "assistant", "content": "ok"},
                                "finish_reason": "stop"
                            }],
                            "model": model,
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        }))
                    }
                },
            ),
        )
        .route("/v1/models", get(|| async { Json(json!({"data": []})) }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let upstream_url = format!("http://{}", addr);
    let proxy_port = free_port().await;
    let config = make_test_proxy_config(proxy_port);
    let accounts = vec![make_test_account("acc-ct", &upstream_url)];

    let mut server = apimanager_lib::proxy::server::start_server(&config, &accounts)
        .await
        .expect("proxy should start");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/v1/messages", proxy_port))
        .header("content-type", "application/json")
        .json(&json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "test"}]
        }))
        .send()
        .await
        .expect("request should succeed");

    assert_eq!(resp.status().as_u16(), 200);

    // Verify the upstream received Content-Type: application/json
    let headers = captured_headers.lock().await;
    let headers = headers.as_ref().expect("should have captured headers");
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("application/json"),
        "Upstream must receive Content-Type: application/json, got: '{}'",
        ct
    );

    server.stop().await;
}
