use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};

/// IP filter middleware stub (Phase 4: will check against whitelist/blacklist).
/// Currently passes all requests through.
pub async fn ip_filter_middleware(
    request: Request,
    next: Next,
) -> Response {
    next.run(request).await
}
