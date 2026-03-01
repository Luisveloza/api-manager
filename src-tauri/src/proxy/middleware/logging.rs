use tower_http::trace::TraceLayer;

/// Create a trace layer for HTTP request logging.
pub fn trace_layer() -> TraceLayer<tower_http::classify::SharedClassifier<tower_http::classify::ServerErrorsAsFailures>> {
    TraceLayer::new_for_http()
}
