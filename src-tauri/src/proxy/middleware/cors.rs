use tower_http::cors::CorsLayer;

/// Create a permissive CORS layer for the proxy server.
pub fn cors_layer() -> CorsLayer {
    CorsLayer::permissive()
}
