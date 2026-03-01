use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Middleware that rejects requests when the service is not running.
/// Bypasses check for /health and /healthz endpoints.
pub async fn service_status_middleware(
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = request.uri().path();

    // Always allow health checks
    if path == "/health" || path == "/healthz" {
        return Ok(next.run(request).await);
    }

    // Check if service is running via extension
    let is_running = request
        .extensions()
        .get::<Arc<AtomicBool>>()
        .map(|flag| flag.load(Ordering::Relaxed))
        .unwrap_or(true); // default to running if not set

    if !is_running {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, middleware, routing::get, Router};
    use tower::ServiceExt;

    async fn ok_handler() -> &'static str {
        "OK"
    }

    #[tokio::test]
    async fn service_status_allows_health() {
        let app = Router::new()
            .route("/health", get(ok_handler))
            .layer(middleware::from_fn(service_status_middleware));

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
