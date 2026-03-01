use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::models::{AppConfig, SiteAccount};
use crate::modules::{backup, browser_storage, config};
use crate::proxy::monitor::ProxyMonitor;
use crate::proxy::server::ProxyServerHandle;
use crate::proxy::token_manager::TokenManager;

/// Managed state for proxy server lifecycle.
#[derive(Clone)]
pub struct ProxyServiceState {
    pub server: Arc<Mutex<ProxyServerHandle>>,
    pub monitor: Arc<tokio::sync::RwLock<Option<Arc<ProxyMonitor>>>>,
    pub token_manager: Arc<tokio::sync::RwLock<Option<Arc<TokenManager>>>>,
}

impl ProxyServiceState {
    pub fn new() -> Self {
        Self {
            server: Arc::new(Mutex::new(ProxyServerHandle::new())),
            monitor: Arc::new(tokio::sync::RwLock::new(None)),
            token_manager: Arc::new(tokio::sync::RwLock::new(None)),
        }
    }
}

// ============================================================================
// Tauri Commands
// ============================================================================

#[tauri::command]
pub async fn import_backup(path: String) -> Result<Vec<crate::models::SiteAccount>, String> {
    let mut accounts = backup::import_backup_from_path(std::path::Path::new(&path))?;
    fetch_api_keys_for_accounts(&mut accounts).await;
    Ok(accounts)
}

#[tauri::command]
pub async fn import_backup_from_text(json: String) -> Result<Vec<crate::models::SiteAccount>, String> {
    let mut accounts = backup::import_backup_from_str(&json)?;
    fetch_api_keys_for_accounts(&mut accounts).await;
    Ok(accounts)
}

#[tauri::command]
pub async fn detect_browser_extension() -> Result<serde_json::Value, String> {
    let dirs = browser_storage::discover_extension_dirs();

    let profiles: Vec<serde_json::Value> = dirs
        .iter()
        .map(|info| {
            serde_json::json!({
                "profile_name": info.profile_name,
                "extension_id": info.extension_id,
                "path": info.path.display().to_string(),
            })
        })
        .collect();

    let found = !dirs.is_empty();
    let extension_id = dirs.first().map(|d| d.extension_id.as_str()).unwrap_or("");

    Ok(serde_json::json!({
        "found": found,
        "profiles": profiles,
        "extension_id": extension_id,
    }))
}

#[tauri::command]
pub async fn sync_from_browser() -> Result<Vec<SiteAccount>, String> {
    // 1. Read raw JSON from Chrome LevelDB
    let raw_json = browser_storage::read_accounts_from_browser()?;

    // 2. Parse the AccountStorageConfig to extract the accounts array.
    //    The structure is: { "configVersion": N, "accounts": [...], ... }
    let storage_config: serde_json::Value = serde_json::from_str(&raw_json)
        .map_err(|e| format!("Failed to parse extension storage JSON: {}", e))?;

    let raw_accounts = storage_config
        .get("accounts")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            "Extension storage does not contain an 'accounts' array".to_string()
        })?;

    // 3. Normalize into SiteAccount structs (reuses backup.rs logic)
    let mut accounts = backup::normalize_accounts(raw_accounts)?;

    if accounts.is_empty() {
        return Err("No accounts found in extension storage".to_string());
    }

    // 4. Fetch API keys for all accounts
    fetch_api_keys_for_accounts(&mut accounts).await;

    tracing::info!(
        count = accounts.len(),
        "Successfully synced accounts from browser extension"
    );

    Ok(accounts)
}

/// Fetch API Keys for all accounts using their access_tokens.
///
/// This calls `GET /api/token/` on each upstream to retrieve the actual `sk-xxx`
/// key needed for AI API calls. The access_token is only a management credential.
///
/// Errors are logged but non-fatal — accounts without keys can still be
/// imported; they just won't be usable for proxying until keys are fetched.
async fn fetch_api_keys_for_accounts(accounts: &mut [SiteAccount]) {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to create HTTP client for key fetching: {}", e);
            return;
        }
    };

    let mut handles = Vec::with_capacity(accounts.len());

    for (idx, account) in accounts.iter().enumerate() {
        let client = client.clone();
        let site_url = account.site_url.clone();
        let site_type = account.site_type.clone();
        let access_token = account.account_info.access_token.clone();
        let user_id = account.account_info.id;
        let account_id = account.id.clone();

        handles.push(tokio::spawn(async move {
            let result = crate::proxy::key_fetcher::fetch_api_key(
                &client,
                &site_url,
                &site_type,
                &access_token,
                user_id,
            )
            .await;
            (idx, account_id, result)
        }));
    }

    for h in handles {
        if let Ok((idx, account_id, result)) = h.await {
            match result {
                Ok(api_key) => {
                    tracing::info!(account_id = %account_id, "Fetched API key");
                    accounts[idx].account_info.api_key = Some(api_key);
                }
                Err(e) => {
                    tracing::warn!(account_id = %account_id, error = %e, "Failed to fetch API key");
                }
            }
        }
    }
}

#[tauri::command]
pub async fn load_config() -> Result<AppConfig, String> {
    Ok(config::load_app_config())
}

/// Refresh API Keys for all stored accounts.
///
/// Reads the current config, re-fetches API Keys from each upstream using
/// access_tokens, updates both `accounts` and `proxy_accounts`, persists
/// to disk, and returns a per-account summary.
#[tauri::command]
pub async fn refresh_api_keys() -> Result<serde_json::Value, String> {
    let mut cfg = config::load_app_config();

    // Refresh keys for the full accounts list
    fetch_api_keys_for_accounts(&mut cfg.accounts).await;

    // Propagate updated api_keys into proxy_accounts (they share the same ids)
    let key_map: std::collections::HashMap<String, Option<String>> = cfg
        .accounts
        .iter()
        .map(|a| (a.id.clone(), a.account_info.api_key.clone()))
        .collect();
    for pa in &mut cfg.proxy_accounts {
        if let Some(new_key) = key_map.get(&pa.id) {
            pa.account_info.api_key = new_key.clone();
        }
    }

    // Persist
    config::save_app_config(&cfg)?;

    // Build summary
    let mut success = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let details: Vec<serde_json::Value> = cfg
        .accounts
        .iter()
        .map(|a| {
            let has_key = a.account_info.api_key.is_some();
            let is_sub2api = a.site_type == "sub2api";
            if is_sub2api {
                skipped += 1;
            } else if has_key {
                success += 1;
            } else {
                failed += 1;
            }
            serde_json::json!({
                "id": a.id,
                "site_name": a.site_name,
                "site_type": a.site_type,
                "has_api_key": has_key,
                "api_key_preview": a.account_info.api_key.as_deref().map(|k| {
                    if k.len() > 12 {
                        format!("{}...{}", &k[..8], &k[k.len()-4..])
                    } else {
                        k.to_string()
                    }
                }),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "success": success,
        "failed": failed,
        "skipped": skipped,
        "total": cfg.accounts.len(),
        "accounts": details,
    }))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn save_config(config_data: AppConfig) -> Result<(), String> {
    config::save_app_config(&config_data)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn proxy_start(
    state: tauri::State<'_, ProxyServiceState>,
    config_data: AppConfig,
) -> Result<(), String> {
    // Check if already running (drop guard before await)
    {
        let server = state.server.lock().await;
        if server.is_running() {
            return Err("Proxy is already running".to_string());
        }
    }

    // Start server (no lock held during await)
    let axum_server = crate::proxy::server::start_server(&config_data.proxy, &config_data.proxy_accounts)
        .await
        .map_err(|e| format!("Failed to start proxy: {}", e))?;

    // Capture monitor and token_manager references before moving the server
    let monitor_ref = axum_server.monitor.clone();
    let token_manager_ref = axum_server.token_manager.clone();

    // Set handle
    {
        let mut server = state.server.lock().await;
        server.set_server(axum_server);
    }

    // Store monitor for get_logs access
    {
        let mut monitor = state.monitor.write().await;
        *monitor = Some(monitor_ref);
    }

    // Store token_manager for get_available_models access
    {
        let mut tm = state.token_manager.write().await;
        *tm = Some(token_manager_ref);
    }

    // Persist config
    config::save_app_config(&config_data)?;

    Ok(())
}

#[tauri::command]
pub async fn proxy_stop(state: tauri::State<'_, ProxyServiceState>) -> Result<(), String> {
    let mut server = state.server.lock().await;

    if !server.is_running() {
        return Err("Proxy is not running".to_string());
    }

    server.stop().await;

    // Clear monitor reference
    {
        let mut monitor = state.monitor.write().await;
        *monitor = None;
    }

    // Clear token_manager reference
    {
        let mut tm = state.token_manager.write().await;
        *tm = None;
    }

    Ok(())
}

#[tauri::command]
pub async fn get_proxy_status(
    state: tauri::State<'_, ProxyServiceState>,
) -> Result<serde_json::Value, String> {
    let server = state.server.lock().await;

    Ok(serde_json::json!({
        "running": server.is_running(),
    }))
}

#[tauri::command]
pub async fn get_logs(
    state: tauri::State<'_, ProxyServiceState>,
) -> Result<serde_json::Value, String> {
    let monitor = state.monitor.read().await;

    if let Some(ref mon) = *monitor {
        let logs = mon.get_logs(0, 100);
        Ok(serde_json::json!({
            "total": mon.get_count(),
            "logs": logs,
        }))
    } else {
        Ok(serde_json::json!({
            "total": 0,
            "logs": [],
        }))
    }
}

#[tauri::command]
pub async fn replay_request(
    state: tauri::State<'_, ProxyServiceState>,
    log_id: String,
) -> Result<serde_json::Value, String> {
    let monitor = state.monitor.read().await;

    let mon = monitor.as_ref().ok_or("Proxy not running")?;

    let log = mon
        .get_log(&log_id)
        .ok_or_else(|| format!("Log {} not found", log_id))?;

    let body = log
        .request_body
        .ok_or("No request body captured for this log")?;

    // Read config to get port and API key for auth
    let config = crate::modules::config::load_app_config();
    let port = config.proxy.port;
    let api_key = &config.proxy.api_key;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(config.proxy.request_timeout))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let url = format!("http://127.0.0.1:{}{}", port, log.url);

    let resp = client
        .request(
            reqwest::Method::from_bytes(log.method.as_bytes())
                .unwrap_or(reqwest::Method::POST),
            &url,
        )
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Replay request failed: {}", e))?;

    let status = resp.status().as_u16();
    let resp_body = resp
        .text()
        .await
        .unwrap_or_else(|e| format!("Failed to read response: {}", e));

    Ok(serde_json::json!({
        "status": status,
        "body": resp_body,
    }))
}

#[tauri::command]
pub async fn get_available_models(
    state: tauri::State<'_, ProxyServiceState>,
) -> Result<Vec<String>, String> {
    let cache = crate::proxy::model_cache::global();

    // 1. Fast path: proxy running and has model data in memory
    {
        let tm = state.token_manager.read().await;
        if let Some(ref token_manager) = *tm {
            let models = token_manager.get_all_models().await;
            if !models.is_empty() {
                tracing::debug!(count = models.len(), "Returning models from proxy registry");
                return Ok(models);
            }
        }
    }

    // 2. Fast path: file cache already loaded in memory
    {
        let models = cache.get_all_models().await;
        if !models.is_empty() {
            tracing::debug!(count = models.len(), "Returning models from memory cache");
            return Ok(models);
        }
    }

    // 3. Try loading from disk (first call after startup)
    cache.load_from_disk().await;
    {
        let models = cache.get_all_models().await;
        if !models.is_empty() {
            tracing::info!(count = models.len(), "Returning models from disk cache");
            // Also feed into proxy registry if running
            populate_proxy_registry_from_cache(&state, &cache).await;
            return Ok(models);
        }
    }

    // 4. Slow path: first-ever launch — fetch from upstreams (once only)
    let _guard = match cache.try_acquire_fetch_guard() {
        Some(g) => g,
        None => {
            // Another call is already fetching; wait briefly then return whatever is available
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            return Ok(cache.get_all_models().await);
        }
    };

    // Double-check after acquiring guard (another call may have populated)
    {
        let models = cache.get_all_models().await;
        if !models.is_empty() {
            return Ok(models);
        }
    }

    tracing::info!("No model cache found — fetching from upstreams (first time)");

    let cfg = config::load_app_config();
    let accounts: Vec<&SiteAccount> = cfg
        .proxy_accounts
        .iter()
        .filter(|a| !a.disabled.unwrap_or(false) && !a.account_info.access_token.is_empty())
        .collect();

    if accounts.is_empty() {
        return Ok(Vec::new());
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let mut handles = Vec::with_capacity(accounts.len());
    for account in &accounts {
        let client = client.clone();
        let site_url = account.site_url.clone();
        let access_token = account.account_info.access_token.clone();
        let account_id = account.id.clone();
        let site_type = account.site_type.clone();

        handles.push(tokio::spawn(async move {
            let models = fetch_models_for_account(&client, &site_url, &access_token, &account_id, &site_type).await;
            (account_id, models)
        }));
    }

    let mut all_models = HashSet::new();
    for h in handles {
        if let Ok((account_id, models)) = h.await {
            if !models.is_empty() {
                let set: HashSet<String> = models.iter().cloned().collect();
                cache.set_account_models(&account_id, set).await;
                for m in models {
                    all_models.insert(m);
                }
            }
        }
    }

    // Persist to disk
    cache.save_to_disk();

    let mut sorted: Vec<String> = all_models.into_iter().collect();
    sorted.sort();

    tracing::info!(total_models = sorted.len(), "Initial model fetch complete — cached to disk");

    // Feed into proxy registry if running
    populate_proxy_registry_from_cache(&state, &cache).await;

    Ok(sorted)
}

#[tauri::command]
pub async fn get_proxy_stats() -> Result<serde_json::Value, String> {
    let stats = crate::proxy::proxy_stats::global().get_stats();
    serde_json::to_value(&stats).map_err(|e| format!("Failed to serialize stats: {}", e))
}

/// If proxy is running and its model registry is empty, populate it from the file cache.
async fn populate_proxy_registry_from_cache(
    state: &tauri::State<'_, ProxyServiceState>,
    cache: &crate::proxy::model_cache::ModelCache,
) {
    let tm = state.token_manager.read().await;
    if let Some(ref token_manager) = *tm {
        if token_manager.get_all_models().await.is_empty() {
            token_manager.load_models_from_cache(cache).await;
        }
    }
}

/// Fetch model list from a single account, using the correct endpoint per site type:
///   - new-api / one-api / Veloera / etc. → `/api/user/models` (returns `string[]` in `data`)
///   - one-hub / done-hub → `/api/available_model` (returns `{model_name: {...}}` map)
///   - sub2api → not supported (no model listing endpoint)
///   - fallback → `/v1/models` (OpenAI-compatible, needs API key)
async fn fetch_models_for_account(
    client: &reqwest::Client,
    site_url: &str,
    access_token: &str,
    account_id: &str,
    site_type: &str,
) -> Vec<String> {
    let base = site_url.trim_end_matches('/');

    match site_type {
        "sub2api" => {
            // Sub2API has no model listing endpoint
            Vec::new()
        }
        "one-hub" | "done-hub" => {
            // OneHub/DoneHub: /api/available_model returns {model_name: {details}}
            fetch_models_onehub(client, base, access_token, account_id).await
        }
        _ => {
            // New-API / One-API / Veloera family: try /api/user/models first
            let models = fetch_models_newapi(client, base, access_token, account_id).await;
            if !models.is_empty() {
                return models;
            }
            // Fallback: /v1/models (works for accounts with sk- API keys)
            fetch_models_openai(client, base, access_token, account_id).await
        }
    }
}

/// New-API family: GET /api/user/models → { "data": ["model-a", "model-b", ...] }
async fn fetch_models_newapi(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    account_id: &str,
) -> Vec<String> {
    let url = format!("{}/api/user/models", base);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            if let Ok(body) = r.json::<serde_json::Value>().await {
                // Response shape: { "success": true, "data": ["model-a", "model-b"] }
                if let Some(arr) = body.get("data").and_then(|d| d.as_array()) {
                    let models: Vec<String> = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    if !models.is_empty() {
                        tracing::info!(account_id, model_count = models.len(), "Fetched models via /api/user/models");
                        return models;
                    }
                }
            }
        }
        Ok(r) => {
            tracing::info!(account_id, status = r.status().as_u16(), "/api/user/models failed, will try fallback");
        }
        Err(e) => {
            tracing::warn!(account_id, error = %e, "Failed to connect for /api/user/models");
        }
    }

    Vec::new()
}

/// OneHub/DoneHub: GET /api/available_model → { "data": { "model-name": {...}, ... } }
async fn fetch_models_onehub(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    account_id: &str,
) -> Vec<String> {
    let url = format!("{}/api/available_model", base);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            if let Ok(body) = r.json::<serde_json::Value>().await {
                // Response shape: { "data": { "gpt-4": {...}, "claude-3": {...} } }
                if let Some(obj) = body.get("data").and_then(|d| d.as_object()) {
                    let models: Vec<String> = obj.keys().cloned().collect();
                    if !models.is_empty() {
                        tracing::info!(account_id, model_count = models.len(), "Fetched models via /api/available_model");
                        return models;
                    }
                }
            }
        }
        Ok(r) => {
            tracing::warn!(account_id, status = r.status().as_u16(), "/api/available_model failed");
        }
        Err(e) => {
            tracing::warn!(account_id, error = %e, "Failed to connect for /api/available_model");
        }
    }

    Vec::new()
}

/// Validate an API key by hitting GET /v1/models on the upstream.
///
/// Returns a JSON object: { "valid": bool, "model_count": u32, "error": string|null }
#[tauri::command(rename_all = "snake_case")]
pub async fn validate_api_key(
    site_url: String,
    api_key: String,
    site_type: String,
) -> Result<serde_json::Value, String> {
    let base = site_url.trim_end_matches('/');

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    // Pick the right endpoint per site type
    let url = match site_type.as_str() {
        "one-hub" | "done-hub" => format!("{}/api/available_model", base),
        _ => format!("{}/v1/models", base),
    };

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            if status == 401 || status == 403 {
                return Ok(serde_json::json!({
                    "valid": false,
                    "model_count": 0,
                    "error": format!("Authentication failed (HTTP {})", status),
                }));
            }
            if !r.status().is_success() {
                return Ok(serde_json::json!({
                    "valid": false,
                    "model_count": 0,
                    "error": format!("Upstream returned HTTP {}", status),
                }));
            }

            // Try to parse model list from response
            let model_count = if let Ok(body) = r.json::<serde_json::Value>().await {
                if let Some(arr) = body.get("data").and_then(|d| d.as_array()) {
                    arr.len()
                } else if let Some(obj) = body.get("data").and_then(|d| d.as_object()) {
                    obj.len()
                } else {
                    0
                }
            } else {
                0
            };

            Ok(serde_json::json!({
                "valid": true,
                "model_count": model_count,
                "error": null,
            }))
        }
        Err(e) => {
            let msg = if e.is_timeout() {
                "Connection timed out".to_string()
            } else if e.is_connect() {
                "Failed to connect to upstream".to_string()
            } else {
                format!("Request failed: {}", e)
            };
            Ok(serde_json::json!({
                "valid": false,
                "model_count": 0,
                "error": msg,
            }))
        }
    }
}

/// OpenAI-compatible: GET /v1/models → { "data": [{"id": "model-name"}, ...] }
async fn fetch_models_openai(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    account_id: &str,
) -> Vec<String> {
    let url = format!("{}/v1/models", base);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            if let Ok(body) = r.json::<serde_json::Value>().await {
                if let Some(arr) = body.get("data").and_then(|d| d.as_array()) {
                    let models: Vec<String> = arr
                        .iter()
                        .filter_map(|item| item.get("id").and_then(|id| id.as_str()).map(String::from))
                        .collect();
                    if !models.is_empty() {
                        tracing::info!(account_id, model_count = models.len(), "Fetched models via /v1/models");
                        return models;
                    }
                }
            }
        }
        Ok(r) => {
            let status = r.status().as_u16();
            if status != 401 && status != 403 {
                tracing::warn!(account_id, status, "/v1/models failed");
            }
        }
        Err(e) => {
            tracing::warn!(account_id, error = %e, "Failed to connect for /v1/models");
        }
    }

    Vec::new()
}
