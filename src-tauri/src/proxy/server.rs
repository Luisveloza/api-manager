use axum::{
    extract::{Path, State},
    http::StatusCode,
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, RwLock};

use crate::models::{AppConfig, ProxyConfig, SiteAccount};
use crate::modules::config;
use crate::proxy::handlers::{anthropic, gemini, openai};
use crate::proxy::middleware::{
    auth_middleware, cors_layer, ip_filter_middleware, monitor_middleware,
    service_status_middleware, SecurityConfig,
};
use crate::proxy::model_router::ModelRouter;
use crate::proxy::monitor::ProxyMonitor;
use crate::proxy::token_manager::TokenManager;
use crate::proxy::upstream::UpstreamClient;

/// Shared application state for all handlers.
#[derive(Clone)]
pub struct AppState {
    pub token_manager: Arc<TokenManager>,
    pub upstream: Arc<UpstreamClient>,
    pub monitor: Arc<ProxyMonitor>,
    pub security: Arc<RwLock<SecurityConfig>>,
    pub model_router: Arc<ModelRouter>,
    pub port: u16,
}

/// Proxy server lifecycle manager.
pub struct AxumServer {
    shutdown_tx: Option<oneshot::Sender<()>>,
    pub token_manager: Arc<TokenManager>,
    pub monitor: Arc<ProxyMonitor>,
}

impl AxumServer {
    /// Start the proxy server with the given configuration and accounts.
    pub async fn start(
        config: &ProxyConfig,
        accounts: &[SiteAccount],
    ) -> Result<Self, String> {
        // Build token manager
        let token_manager = Arc::new(TokenManager::with_mode(config.load_balance_mode.clone()));
        token_manager.load_from_accounts(accounts);

        // Build upstream client
        let upstream_proxy = if config.upstream_proxy.enabled {
            Some(&config.upstream_proxy)
        } else {
            None
        };
        let upstream = Arc::new(UpstreamClient::new(
            Duration::from_secs(config.request_timeout),
            upstream_proxy,
        ));

        // Start auto-cleanup (needs upstream for stale model refresh)
        token_manager.start_auto_cleanup(upstream.clone()).await;

        // Preflight check — verify account connectivity in the background
        {
            let tm = token_manager.clone();
            tokio::spawn(async move {
                tm.preflight_check().await;
            });
        }

        // Build monitor
        let monitor = Arc::new(ProxyMonitor::new(1000));
        monitor.set_enabled(config.enable_logging);

        // Build security config
        let security = Arc::new(RwLock::new(SecurityConfig {
            auth_mode: config.auth_mode.clone(),
            api_key: config.api_key.clone(),
            admin_password: config.admin_password.clone(),
            is_headless: false,
            api_keys: config.api_keys.clone(),
        }));

        // Build model router
        let model_router = Arc::new(ModelRouter::new(
            config.model_aliases.clone(),
            config.model_routes.clone(),
        ));

        let state = AppState {
            token_manager: token_manager.clone(),
            upstream: upstream.clone(),
            monitor: monitor.clone(),
            security: security.clone(),
            model_router,
            port: config.port,
        };

        // Load proxy stats from disk
        crate::proxy::proxy_stats::global().load_from_disk();

        // Start price cache background refresh
        {
            let pc = crate::proxy::price_cache::global();
            tokio::spawn(async move {
                pc.refresh().await;
            });
        }

        // Load models from disk cache first (instant, no network).
        // Only fetch from upstreams if cache is empty (first-ever launch).
        {
            let tm = token_manager.clone();
            let up = upstream.clone();
            tokio::spawn(async move {
                let cache = crate::proxy::model_cache::global();
                cache.load_from_disk().await;

                if !cache.is_empty() {
                    tm.load_models_from_cache(&cache).await;
                    tracing::info!("Proxy model registry populated from disk cache");
                } else {
                    tracing::info!("No disk cache — fetching models from upstreams");
                    tm.fetch_models_from_upstreams(&up).await;
                }
            });
        }

        // Build router
        let router = build_router(state, security, monitor.clone());

        // Bind and serve
        let bind_addr = format!("{}:{}", config.get_bind_address(), config.port);
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", bind_addr, e))?;

        tracing::info!("Proxy server listening on {}", bind_addr);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                    tracing::info!("Proxy server shutting down");
                })
                .await
                .ok();
        });

        Ok(Self {
            shutdown_tx: Some(shutdown_tx),
            token_manager,
            monitor,
        })
    }

    /// Stop the proxy server.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.token_manager
            .graceful_shutdown(Duration::from_secs(5))
            .await;
        // graceful_shutdown already flushes proxy_stats
    }

    pub fn is_running(&self) -> bool {
        self.shutdown_tx.is_some()
    }
}

fn build_router(
    state: AppState,
    security: Arc<RwLock<SecurityConfig>>,
    monitor: Arc<ProxyMonitor>,
) -> Router {
    // AI proxy routes (with auth + monitor middleware)
    let ai_routes = Router::new()
        .route("/v1/chat/completions", post(openai::handle_chat_completions))
        .route("/v1/completions", post(openai::handle_completions))
        .route("/v1/models", get(openai::handle_list_models))
        .route("/v1/messages", post(anthropic::handle_messages))
        .route("/v1beta/models", get(gemini::handle_list_models))
        .route(
            "/v1beta/models/:model_action",
            post(gemini::handle_generate),
        )
        .layer(middleware::from_fn_with_state(
            monitor.clone(),
            monitor_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            security.clone(),
            auth_middleware,
        ))
        .with_state(state.clone());

    // Admin/management routes (with admin auth)
    let admin_routes = Router::new()
        .route("/api/config", get(admin_get_config))
        .route("/api/config", post(admin_save_config))
        .route("/api/proxy/status", get(admin_get_proxy_status))
        .route("/api/accounts", get(admin_list_accounts))
        .route("/api/logs", get(admin_get_logs))
        .route("/api/stats/summary", get(admin_get_stats_summary))
        .route("/api/stats/detail", get(admin_get_stats_detail))
        .route("/api/stats/by-model", get(admin_get_stats_by_model))
        .route("/api/stats/timeline", get(admin_get_stats_timeline))
        .route("/api/stats/budget", get(admin_get_stats_budget))
        .route("/api/logs/:log_id", get(admin_get_log_detail))
        .route("/api/logs/:log_id/replay", post(admin_replay_request))
        .route("/api/stats/keys", get(admin_get_stats_by_key))
        .layer(middleware::from_fn_with_state(
            security.clone(),
            crate::proxy::middleware::admin_auth_middleware,
        ))
        .with_state(state.clone());

    // Health routes (no auth)
    let health_routes = Router::new()
        .route("/health", get(health_check))
        .route("/healthz", get(health_check));

    // Combine all routes with global middleware
    Router::new()
        .merge(health_routes)
        .merge(ai_routes)
        .merge(admin_routes)
        .layer(middleware::from_fn(service_status_middleware))
        .layer(middleware::from_fn(ip_filter_middleware))
        .layer(cors_layer())
}

// ============================================================================
// Health Check
// ============================================================================

async fn health_check() -> &'static str {
    "OK"
}

// ============================================================================
// Admin Handlers
// ============================================================================

async fn admin_get_config() -> impl IntoResponse {
    let config = config::load_app_config();
    Json(config)
}

async fn admin_save_config(
    Json(config_data): Json<AppConfig>,
) -> Result<impl IntoResponse, StatusCode> {
    config::save_app_config(&config_data).map_err(|e| {
        tracing::error!("Failed to save config: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(serde_json::json!({"status": "ok"})))
}

async fn admin_get_proxy_status(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "running": true,
        "port": state.port,
        "token_count": state.token_manager.token_count(),
        "monitor_enabled": state.monitor.is_enabled(),
        "log_count": state.monitor.get_count(),
    }))
}

async fn admin_list_accounts(State(state): State<AppState>) -> impl IntoResponse {
    let config = config::load_app_config();
    Json(serde_json::json!({
        "total_accounts": config.accounts.len(),
        "proxy_accounts": config.proxy_accounts.len(),
        "active_tokens": state.token_manager.token_count(),
    }))
}

async fn admin_get_logs(State(state): State<AppState>) -> impl IntoResponse {
    let logs = state.monitor.get_logs(0, 100);
    Json(serde_json::json!({
        "total": state.monitor.get_count(),
        "logs": logs,
    }))
}

async fn admin_get_stats_summary(State(state): State<AppState>) -> impl IntoResponse {
    let stats = crate::proxy::proxy_stats::global().get_stats();
    Json(serde_json::json!({
        "total_requests": stats.global.total_requests,
        "active_tokens": state.token_manager.token_count(),
        "total_cost": stats.global.total_estimated_cost,
        "success_rate": stats.global.success_rate(),
        "avg_latency_ms": stats.global.avg_latency_ms(),
    }))
}

async fn admin_get_stats_detail() -> impl IntoResponse {
    let stats = crate::proxy::proxy_stats::global().get_stats();
    Json(serde_json::to_value(&stats).unwrap_or_default())
}

async fn admin_get_stats_by_model() -> impl IntoResponse {
    let items = crate::proxy::proxy_stats::global().stats_by_model(20);
    let data: Vec<serde_json::Value> = items
        .into_iter()
        .map(|(model, stats)| {
            serde_json::json!({
                "model": model,
                "total_requests": stats.total_requests,
                "success_count": stats.success_count,
                "error_count": stats.error_count,
                "total_input_tokens": stats.total_input_tokens,
                "total_output_tokens": stats.total_output_tokens,
                "total_estimated_cost": stats.total_estimated_cost,
                "avg_latency_ms": stats.avg_latency_ms(),
            })
        })
        .collect();
    Json(serde_json::json!({ "data": data }))
}

async fn admin_get_stats_timeline() -> impl IntoResponse {
    let timeline = crate::proxy::proxy_stats::global().stats_timeline();
    Json(serde_json::json!({ "data": timeline }))
}

async fn admin_get_stats_budget() -> impl IntoResponse {
    let proxy_config = config::load_app_config().proxy;
    let today_cost = crate::proxy::proxy_stats::global().today_total_cost();
    let total_cost = crate::proxy::proxy_stats::global().get_stats().global.total_estimated_cost;
    let daily_limit = proxy_config.daily_cost_limit;
    let monthly_limit = proxy_config.monthly_cost_limit;
    Json(serde_json::json!({
        "today_cost": today_cost,
        "total_cost": total_cost,
        "daily_limit": daily_limit,
        "monthly_limit": monthly_limit,
        "daily_exceeded": daily_limit > 0.0 && today_cost >= daily_limit,
        "monthly_exceeded": monthly_limit > 0.0 && total_cost >= monthly_limit,
        "action": proxy_config.budget_exceeded_action,
    }))
}

async fn admin_get_log_detail(
    State(state): State<AppState>,
    Path(log_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    state
        .monitor
        .get_log(&log_id)
        .map(|log| Json(serde_json::to_value(&log).unwrap_or_default()))
        .ok_or(StatusCode::NOT_FOUND)
}

async fn admin_replay_request(
    State(state): State<AppState>,
    Path(log_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let log = state
        .monitor
        .get_log(&log_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let body = log
        .request_body
        .ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;

    // Re-send the request through the proxy's upstream client using the same path
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );

    // Pick any available token for the replay
    let token = state
        .token_manager
        .get_token_excluding(
            None,
            log.model.as_deref(),
            None,
            &[],
        )
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let result = state
        .upstream
        .forward(
            &token.site_url,
            &log.url,
            reqwest::Method::from_bytes(log.method.as_bytes())
                .unwrap_or(reqwest::Method::POST),
            headers,
            bytes::Bytes::from(body),
            token.upstream_credential(),
        )
        .await;

    match result {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let resp_body = resp.text().await.unwrap_or_default();
            Ok(Json(serde_json::json!({
                "status": status,
                "body": resp_body,
            })))
        }
        Err(e) => Ok(Json(serde_json::json!({
            "status": 502,
            "body": format!("Replay failed: {}", e),
        }))),
    }
}

async fn admin_get_stats_by_key() -> impl IntoResponse {
    let all = crate::proxy::proxy_stats::global().all_key_stats();
    let data: Vec<serde_json::Value> = all
        .into_iter()
        .map(|(key, stats)| {
            // Mask key for display: show first 8 and last 4 chars
            let masked = if key.len() > 12 {
                format!("{}...{}", &key[..8], &key[key.len() - 4..])
            } else {
                key.clone()
            };
            serde_json::json!({
                "key": key,
                "masked_key": masked,
                "total_requests": stats.total_requests,
                "total_cost": stats.total_cost,
                "today_cost": stats.today_cost,
            })
        })
        .collect();
    Json(serde_json::json!({ "data": data }))
}

// ============================================================================
// Legacy ProxyServerHandle (kept for Tauri command compatibility)
// ============================================================================

/// Proxy server state wrapper for Tauri managed state.
pub struct ProxyServerHandle {
    server: Option<AxumServer>,
}

impl ProxyServerHandle {
    pub fn new() -> Self {
        Self { server: None }
    }

    pub fn is_running(&self) -> bool {
        self.server.as_ref().map(|s| s.is_running()).unwrap_or(false)
    }

    pub fn set_server(&mut self, server: AxumServer) {
        self.server = Some(server);
    }

    pub async fn stop(&mut self) {
        if let Some(server) = self.server.as_mut() {
            server.stop().await;
        }
        self.server = None;
    }
}

/// Start the proxy server (convenience function used by Tauri commands).
pub async fn start_server(
    config: &ProxyConfig,
    accounts: &[SiteAccount],
) -> Result<AxumServer, String> {
    AxumServer::start(config, accounts).await
}
