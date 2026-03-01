use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::models::{ProxyApiKey, ProxyAuthMode};

/// Security configuration shared across middleware.
#[derive(Debug, Clone)]
pub struct SecurityConfig {
    pub auth_mode: ProxyAuthMode,
    pub api_key: String,
    pub admin_password: Option<String>,
    pub is_headless: bool,
    pub api_keys: Vec<ProxyApiKey>,
}

/// Extension type injected into requests authenticated via a user API key.
/// Contains the key label for downstream logging/stats.
#[derive(Clone, Debug)]
pub struct AuthenticatedKey {
    pub label: String,
    pub key: String,
}

impl SecurityConfig {
    /// Resolve the effective auth mode (Auto → concrete mode).
    pub fn effective_auth_mode(&self) -> ProxyAuthMode {
        match self.auth_mode {
            ProxyAuthMode::Auto => {
                if self.is_headless {
                    ProxyAuthMode::AllExceptHealth
                } else {
                    ProxyAuthMode::Off
                }
            }
            ref mode => mode.clone(),
        }
    }
}

/// Extract API token from request headers.
/// Supports multiple authentication schemes:
///   1. `Authorization: Bearer <token>` (OpenAI / Codex style)
///   2. `x-api-key: <token>` (Anthropic / Claude Code style)
///   3. `?key=<token>` query parameter (Gemini CLI style)
fn extract_api_token(request: &Request) -> Option<String> {
    // 1. Authorization: Bearer
    if let Some(token) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.to_string())
    {
        return Some(token);
    }

    // 2. x-api-key (Anthropic SDK)
    if let Some(token) = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|t| t.to_string())
    {
        return Some(token);
    }

    // 3. ?key= query parameter (Gemini CLI)
    if let Some(query) = request.uri().query() {
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("key=") {
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }

    None
}

/// Auth middleware for API routes.
///
/// Based on ProxyAuthMode:
/// - Off: all requests pass
/// - Strict: all requests require valid api_key
/// - AllExceptHealth: /health and /healthz bypass, rest require api_key
/// - Auto: Desktop → Off, Headless → AllExceptHealth
pub async fn auth_middleware(
    State(security): State<Arc<RwLock<SecurityConfig>>>,
    mut request: Request,
    next: Next,
) -> Result<Response, Response> {
    let config = security.read().await;
    let mode = config.effective_auth_mode();

    match mode {
        ProxyAuthMode::Off => Ok(next.run(request).await),
        ProxyAuthMode::Strict => {
            validate_token(&config, &mut request)?;
            drop(config);
            Ok(next.run(request).await)
        }
        ProxyAuthMode::AllExceptHealth | ProxyAuthMode::Auto => {
            let path = request.uri().path();
            if path == "/health" || path == "/healthz" {
                drop(config);
                return Ok(next.run(request).await);
            }

            validate_token(&config, &mut request)?;
            drop(config);
            Ok(next.run(request).await)
        }
    }
}

/// Validate the request token against the global api_key and user api_keys.
/// If matched via a user key, injects `AuthenticatedKey` into the request extensions.
fn validate_token(config: &SecurityConfig, request: &mut Request) -> Result<(), Response> {
    let token = extract_api_token(request).ok_or_else(unauthorized_response)?;

    // 1. Check global admin key first
    if token == config.api_key {
        return Ok(());
    }

    // 2. Check user API keys
    for user_key in &config.api_keys {
        if user_key.key == token {
            if !user_key.enabled {
                return Err(forbidden_response("API key is disabled"));
            }

            // Check per-key cost limits
            let stats = crate::proxy::proxy_stats::global().get_per_key_stats(&user_key.key);
            if user_key.daily_limit > 0.0 && stats.today_cost >= user_key.daily_limit {
                return Err(quota_exceeded_response("Daily cost limit exceeded for this API key"));
            }
            if user_key.monthly_limit > 0.0 && stats.total_cost >= user_key.monthly_limit {
                return Err(quota_exceeded_response("Monthly cost limit exceeded for this API key"));
            }

            // Inject the matched key info for downstream use
            request.extensions_mut().insert(AuthenticatedKey {
                label: user_key.label.clone(),
                key: user_key.key.clone(),
            });
            return Ok(());
        }
    }

    Err(unauthorized_response())
}

/// Admin auth middleware for management routes.
/// Always requires admin_password (falls back to api_key if admin_password is None).
pub async fn admin_auth_middleware(
    State(security): State<Arc<RwLock<SecurityConfig>>>,
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    let config = security.read().await;

    let token = extract_api_token(&request);
    let expected = config
        .admin_password
        .as_deref()
        .unwrap_or(&config.api_key);

    if token.as_deref() == Some(expected) {
        drop(config);
        Ok(next.run(request).await)
    } else {
        Err(unauthorized_response())
    }
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        serde_json::json!({
            "error": {
                "message": "Invalid API key",
                "type": "authentication_error",
                "code": "invalid_api_key"
            }
        })
        .to_string(),
    )
        .into_response()
}

fn forbidden_response(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        serde_json::json!({
            "error": {
                "message": message,
                "type": "authentication_error",
                "code": "api_key_disabled"
            }
        })
        .to_string(),
    )
        .into_response()
}

fn quota_exceeded_response(message: &str) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        serde_json::json!({
            "error": {
                "message": message,
                "type": "rate_limit_error",
                "code": "quota_exceeded"
            }
        })
        .to_string(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, middleware, routing::get, Router};
    use tower::ServiceExt;

    fn make_security(mode: ProxyAuthMode) -> Arc<RwLock<SecurityConfig>> {
        Arc::new(RwLock::new(SecurityConfig {
            auth_mode: mode,
            api_key: "sk-test-key-123".to_string(),
            admin_password: None,
            is_headless: false,
            api_keys: vec![],
        }))
    }

    async fn ok_handler() -> &'static str {
        "OK"
    }

    fn build_app(security: Arc<RwLock<SecurityConfig>>) -> Router {
        Router::new()
            .route("/v1/chat/completions", get(ok_handler))
            .route("/health", get(ok_handler))
            .layer(middleware::from_fn_with_state(
                security.clone(),
                auth_middleware,
            ))
            .with_state(security)
    }

    #[tokio::test]
    async fn auth_passes_valid_key() {
        let security = make_security(ProxyAuthMode::Strict);
        let app = build_app(security);

        let req = Request::builder()
            .uri("/v1/chat/completions")
            .header("Authorization", "Bearer sk-test-key-123")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_rejects_invalid_key() {
        let security = make_security(ProxyAuthMode::Strict);
        let app = build_app(security);

        let req = Request::builder()
            .uri("/v1/chat/completions")
            .header("Authorization", "Bearer wrong-key")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_skips_health() {
        let security = make_security(ProxyAuthMode::AllExceptHealth);
        let app = build_app(security);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_off_allows_all() {
        let security = make_security(ProxyAuthMode::Off);
        let app = build_app(security);

        let req = Request::builder()
            .uri("/v1/chat/completions")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_passes_x_api_key() {
        let security = make_security(ProxyAuthMode::Strict);
        let app = build_app(security);

        let req = Request::builder()
            .uri("/v1/chat/completions")
            .header("x-api-key", "sk-test-key-123")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_passes_query_key() {
        let security = make_security(ProxyAuthMode::Strict);
        let app = build_app(security);

        let req = Request::builder()
            .uri("/v1/chat/completions?key=sk-test-key-123")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
