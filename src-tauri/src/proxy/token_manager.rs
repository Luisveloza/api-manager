use dashmap::DashMap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::models::{LoadBalanceMode, ProxyHealthState, SiteAccount};
use crate::proxy::circuit_breaker::{CircuitBreakerConfig, CircuitBreakerRegistry};
use crate::proxy::rate_limit::RateLimitTracker;
use crate::proxy::upstream::UpstreamClient;

/// A token representing a proxied upstream account.
#[derive(Debug, Clone)]
pub struct ProxyToken {
    pub account_id: String,
    pub site_url: String,
    pub site_name: String,
    pub site_type: String,
    /// Management-plane credential (used for listing models, fetching keys, etc.)
    pub access_token: String,
    /// Data-plane credential (the actual `sk-xxx` used for AI API calls).
    /// `None` for sub2api where the JWT access_token doubles as the API key.
    pub api_key: Option<String>,
    pub remaining_quota: Option<f64>,
    /// Priority for failover load balancing (lower = higher priority).
    pub priority: i32,
    /// Weight for weighted load balancing (1-100).
    pub weight: u32,
}

impl ProxyToken {
    /// Create a ProxyToken from a SiteAccount.
    pub fn from_site_account(account: &SiteAccount) -> Self {
        Self {
            account_id: account.id.clone(),
            site_url: account.site_url.clone(),
            site_name: account.site_name.clone(),
            site_type: account.site_type.clone(),
            access_token: account.account_info.access_token.clone(),
            api_key: account.account_info.api_key.clone(),
            remaining_quota: Some(account.account_info.quota),
            priority: account.proxy_priority,
            weight: account.proxy_weight,
        }
    }

    /// Returns the credential to use for upstream AI API calls.
    ///
    /// For most site types this is `api_key` (fetched via `GET /api/token/`).
    /// Falls back to `access_token` when no separate API key is available
    /// (e.g. sub2api where the JWT is the API key).
    pub fn upstream_credential(&self) -> &str {
        self.api_key.as_deref().unwrap_or(&self.access_token)
    }
}

/// Per-account model sets + aggregated model list.
struct ModelRegistry {
    /// account_id -> set of model ids this account supports.
    account_models: DashMap<String, HashSet<String>>,
    /// De-duplicated, sorted list of all models across accounts.
    all_models: RwLock<Vec<String>>,
}

impl ModelRegistry {
    fn new() -> Self {
        Self {
            account_models: DashMap::new(),
            all_models: RwLock::new(Vec::new()),
        }
    }

    /// Returns true if the account is known to support the given model.
    /// If we have no record for the account, return true (degrade gracefully).
    fn supports_model(&self, account_id: &str, model: &str) -> bool {
        match self.account_models.get(account_id) {
            Some(models) => models.contains(model),
            None => true, // no data yet — don't filter
        }
    }

    /// Remove a specific model from an account's supported model set.
    /// Called when upstream returns 404 for a model — prevents the same account
    /// from being selected for that model again until the next full refresh.
    fn remove_model_for_account(&self, account_id: &str, model: &str) {
        if let Some(mut models) = self.account_models.get_mut(account_id) {
            models.remove(model);
        }
    }

    /// Rebuild the aggregated all_models list from per-account data.
    async fn rebuild_aggregated(&self) {
        let mut merged = HashSet::new();
        for entry in self.account_models.iter() {
            for m in entry.value() {
                merged.insert(m.clone());
            }
        }
        let mut sorted: Vec<String> = merged.into_iter().collect();
        sorted.sort();
        let mut guard = self.all_models.write().await;
        *guard = sorted;
    }
}

/// Session entry with TTL.
#[derive(Debug, Clone)]
struct SessionEntry {
    account_id: String,
    created_at: Instant,
}

/// Session TTL: 30 minutes.
const SESSION_TTL: Duration = Duration::from_secs(30 * 60);

/// Manages the pool of proxy tokens with round-robin scheduling,
/// rate limit awareness, circuit breaker health, and sticky sessions.
pub struct TokenManager {
    tokens: Arc<DashMap<String, ProxyToken>>,
    current_index: Arc<AtomicUsize>,
    rate_limit_tracker: Arc<RateLimitTracker>,
    session_accounts: Arc<DashMap<String, SessionEntry>>,
    preferred_account_id: Arc<RwLock<Option<String>>>,
    circuit_breaker: Arc<CircuitBreakerRegistry>,
    cancel_token: CancellationToken,
    auto_cleanup_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    model_registry: Arc<ModelRegistry>,
    load_balance_mode: LoadBalanceMode,
}

impl TokenManager {
    pub fn new() -> Self {
        Self::with_mode(LoadBalanceMode::RoundRobin)
    }

    pub fn with_mode(mode: LoadBalanceMode) -> Self {
        Self {
            tokens: Arc::new(DashMap::new()),
            current_index: Arc::new(AtomicUsize::new(0)),
            rate_limit_tracker: Arc::new(RateLimitTracker::new()),
            session_accounts: Arc::new(DashMap::new()),
            preferred_account_id: Arc::new(RwLock::new(None)),
            circuit_breaker: Arc::new(CircuitBreakerRegistry::new(CircuitBreakerConfig::default())),
            cancel_token: CancellationToken::new(),
            auto_cleanup_handle: Arc::new(tokio::sync::Mutex::new(None)),
            model_registry: Arc::new(ModelRegistry::new()),
            load_balance_mode: mode,
        }
    }

    /// Load tokens from a slice of SiteAccounts.
    /// No longer permanently skips proxy-disabled accounts — they're loaded with
    /// their circuit breaker state (Open), which will auto-recover after cooldown.
    pub fn load_from_accounts(&self, accounts: &[SiteAccount]) {
        self.tokens.clear();
        self.current_index.store(0, Ordering::SeqCst);

        let mut loaded = 0u32;
        let mut loaded_open = 0u32;

        for account in accounts {
            if account.disabled.unwrap_or(false) {
                continue;
            }
            if account.account_info.access_token.is_empty() {
                continue;
            }
            // sub2api: JWT access_token doubles as the API key, no separate api_key needed.
            // All other site types: must have api_key to call AI API endpoints.
            if account.site_type != "sub2api"
                && account
                    .account_info
                    .api_key
                    .as_deref()
                    .unwrap_or("")
                    .is_empty()
            {
                tracing::warn!(
                    account_id = %account.id,
                    site_name = %account.site_name,
                    "Skipping account: no api_key (run 'Refresh API Keys' first)"
                );
                continue;
            }

            let token = ProxyToken::from_site_account(account);

            // Initialize circuit breaker from persisted state
            if let Some(ref ph) = account.proxy_health {
                self.circuit_breaker.load_persisted(
                    &token.account_id,
                    ph.circuit_state.as_deref(),
                    ph.consecutive_failures,
                    ph.trip_count,
                    &ph.failure_reason,
                    ph.disabled_by_proxy,
                );
                if ph.disabled_by_proxy || ph.circuit_state.as_deref() == Some("open") {
                    loaded_open += 1;
                }
            }

            self.tokens.insert(token.account_id.clone(), token);
            loaded += 1;
        }

        tracing::info!(
            loaded,
            loaded_open,
            "Token pool loaded"
        );
    }

    /// Select a token using round-robin, respecting rate limits, health, model support,
    /// and protocol compatibility.
    ///
    /// `protocol` should be `Some("openai")`, `Some("anthropic")`, or `Some("gemini")`.
    /// When set, accounts whose `site_type` is known to be incompatible with the
    /// requested protocol are skipped.
    ///
    /// `exclude` lists account IDs that have already failed in the current request's
    /// retry loop and should not be selected again.
    pub fn get_token(
        &self,
        session_id: Option<&str>,
        model: Option<&str>,
        protocol: Option<&str>,
    ) -> Option<ProxyToken> {
        self.get_token_excluding(session_id, model, protocol, &[])
    }

    /// Like `get_token`, but also skips accounts whose IDs are in `exclude`.
    pub fn get_token_excluding(
        &self,
        session_id: Option<&str>,
        model: Option<&str>,
        protocol: Option<&str>,
        exclude: &[String],
    ) -> Option<ProxyToken> {
        let pool_size = self.tokens.len();
        if pool_size == 0 {
            return None;
        }

        // 1. Check preferred account
        if let Ok(guard) = self.preferred_account_id.try_read() {
            if let Some(ref preferred_id) = *guard {
                if !exclude.iter().any(|e| e == preferred_id) {
                    if let Some(token) = self.tokens.get(preferred_id) {
                        if !self.circuit_breaker.is_tripped(&token.account_id)
                            && !self.rate_limit_tracker.is_limited(&token.account_id)
                            && model
                                .map(|m| self.model_registry.supports_model(&token.account_id, m))
                                .unwrap_or(true)
                            && protocol
                                .map(|p| is_compatible(&token.site_type, p))
                                .unwrap_or(true)
                        {
                            return Some(token.clone());
                        }
                    }
                }
            }
        }

        // 2. Check sticky session (with TTL)
        if let Some(sid) = session_id {
            if let Some(entry) = self.session_accounts.get(sid) {
                if entry.created_at.elapsed() < SESSION_TTL {
                    if !exclude.iter().any(|e| e.as_str() == entry.account_id.as_str()) {
                        if let Some(token) = self.tokens.get(&entry.account_id) {
                            if !self.circuit_breaker.is_tripped(&token.account_id)
                                && !self.rate_limit_tracker.is_limited(&token.account_id)
                                && model
                                    .map(|m| self.model_registry.supports_model(&token.account_id, m))
                                    .unwrap_or(true)
                                && protocol
                                    .map(|p| is_compatible(&token.site_type, p))
                                    .unwrap_or(true)
                            {
                                return Some(token.clone());
                            }
                        }
                    }
                } else {
                    // TTL expired
                    self.session_accounts.remove(sid);
                }
            }
        }

        // 3. Build ordered key list based on load balance mode
        //
        // Weighted mode: do weighted random selection directly and return early.
        if self.load_balance_mode == LoadBalanceMode::Weighted {
            return self.weighted_select(exclude, model, protocol, session_id);
        }

        let keys: Vec<String> = match self.load_balance_mode {
            LoadBalanceMode::Failover => {
                let mut entries: Vec<(String, i32)> = self
                    .tokens
                    .iter()
                    .map(|r| (r.key().clone(), r.value().priority))
                    .collect();
                entries.sort_by_key(|(_, p)| *p);
                entries.into_iter().map(|(k, _)| k).collect()
            }
            LoadBalanceMode::Random => {
                use rand::seq::SliceRandom;
                let mut keys: Vec<String> = self.tokens.iter().map(|r| r.key().clone()).collect();
                keys.shuffle(&mut rand::thread_rng());
                keys
            }
            LoadBalanceMode::RoundRobin => {
                self.tokens.iter().map(|r| r.key().clone()).collect()
            }
            // Weighted is handled above via early return; this arm is unreachable.
            LoadBalanceMode::Weighted => unreachable!(),
        };

        if keys.is_empty() {
            return None;
        }

        let start_idx = match self.load_balance_mode {
            LoadBalanceMode::RoundRobin => {
                self.current_index.fetch_add(1, Ordering::Relaxed) % keys.len()
            }
            _ => 0, // Failover and Random scan from the beginning
        };

        for i in 0..keys.len() {
            let idx = (start_idx + i) % keys.len();
            let key = &keys[idx];

            if let Some(token) = self.tokens.get(key) {
                // Skip excluded (already failed in this retry loop)
                if exclude.iter().any(|e| e.as_str() == token.account_id.as_str()) {
                    continue;
                }
                // Skip tripped (circuit breaker)
                if self.circuit_breaker.is_tripped(&token.account_id) {
                    continue;
                }
                // Skip rate-limited
                if self.rate_limit_tracker.is_limited(&token.account_id) {
                    continue;
                }
                // Skip accounts that don't support the requested model
                if let Some(m) = model {
                    if !self.model_registry.supports_model(&token.account_id, m) {
                        continue;
                    }
                }
                // Skip accounts incompatible with the requested protocol
                if let Some(p) = protocol {
                    if !is_compatible(&token.site_type, p) {
                        continue;
                    }
                }

                // Bind to session if applicable
                if let Some(sid) = session_id {
                    self.session_accounts.insert(
                        sid.to_string(),
                        SessionEntry {
                            account_id: token.account_id.clone(),
                            created_at: Instant::now(),
                        },
                    );
                }

                return Some(token.clone());
            }
        }

        tracing::warn!(
            pool_size = keys.len(),
            excluded = exclude.len(),
            model = model.unwrap_or("(none)"),
            protocol = protocol.unwrap_or("(none)"),
            "get_token_excluding: no suitable account found after scanning entire pool"
        );
        None
    }

    /// Weighted random selection: pick an account proportional to its weight.
    fn weighted_select(
        &self,
        exclude: &[String],
        model: Option<&str>,
        protocol: Option<&str>,
        session_id: Option<&str>,
    ) -> Option<ProxyToken> {
        use rand::Rng;

        // Collect eligible candidates with their weights.
        let candidates: Vec<ProxyToken> = self
            .tokens
            .iter()
            .filter(|r| {
                let token = r.value();
                !exclude.iter().any(|e| e.as_str() == token.account_id.as_str())
                    && !self.circuit_breaker.is_tripped(&token.account_id)
                    && !self.rate_limit_tracker.is_limited(&token.account_id)
                    && model
                        .map(|m| self.model_registry.supports_model(&token.account_id, m))
                        .unwrap_or(true)
                    && protocol
                        .map(|p| is_compatible(&token.site_type, p))
                        .unwrap_or(true)
            })
            .map(|r| r.value().clone())
            .collect();

        if candidates.is_empty() {
            tracing::warn!(
                excluded = exclude.len(),
                model = model.unwrap_or("(none)"),
                protocol = protocol.unwrap_or("(none)"),
                "weighted_select: no eligible candidates"
            );
            return None;
        }

        let total_weight: u64 = candidates.iter().map(|t| t.weight.max(1) as u64).sum();
        let mut rng = rand::thread_rng();
        let roll = rng.gen_range(0..total_weight);

        let mut cumulative: u64 = 0;
        for token in &candidates {
            cumulative += token.weight.max(1) as u64;
            if roll < cumulative {
                // Bind to session if applicable
                if let Some(sid) = session_id {
                    self.session_accounts.insert(
                        sid.to_string(),
                        SessionEntry {
                            account_id: token.account_id.clone(),
                            created_at: Instant::now(),
                        },
                    );
                }
                return Some(token.clone());
            }
        }

        // Fallback (shouldn't reach here)
        candidates.last().cloned()
    }

    /// Concurrently fetch models from each account's upstream and populate the model registry.
    /// Uses the appropriate endpoint per site type:
    ///   - new-api / one-api family → /api/user/models (fallback to /v1/models)
    ///   - one-hub / done-hub → /api/available_model
    ///   - sub2api → skip (no model listing endpoint)
    /// Skips accounts that are currently rate-limited or circuit-tripped.
    /// Results are persisted to the global file cache.
    pub async fn fetch_models_from_upstreams(&self, upstream: &Arc<UpstreamClient>) {
        let entries: Vec<ProxyToken> = self
            .tokens
            .iter()
            .filter(|r| {
                let id = r.key();
                !self.rate_limit_tracker.is_limited(id) && !self.circuit_breaker.is_tripped(id)
            })
            .map(|r| r.value().clone())
            .collect();
        if entries.is_empty() {
            return;
        }

        let cache = crate::proxy::model_cache::global();

        let mut handles = Vec::with_capacity(entries.len());
        for token in entries {
            let upstream = upstream.clone();
            let registry = self.model_registry.clone();
            let cache = cache.clone();
            handles.push(tokio::spawn(async move {
                let models = fetch_models_for_token(
                    &upstream,
                    &token,
                )
                .await;
                if !models.is_empty() {
                    let count = models.len();
                    registry.account_models.insert(token.account_id.clone(), models.clone());
                    cache.set_account_models(&token.account_id, models).await;
                    tracing::info!(
                        account_id = %token.account_id,
                        model_count = count,
                        "Loaded models for account"
                    );
                }
            }));
        }

        for h in handles {
            let _ = h.await;
        }

        self.model_registry.rebuild_aggregated().await;

        // Persist to disk
        cache.save_to_disk();

        let count = self.model_registry.all_models.read().await.len();
        tracing::info!(total_models = count, "Model registry built");
    }

    /// Fetch models only for the specified accounts (used for stale refresh).
    pub async fn fetch_models_for_accounts(&self, upstream: &Arc<UpstreamClient>, account_ids: &[String]) {
        let cache = crate::proxy::model_cache::global();

        let mut handles = Vec::new();
        for account_id in account_ids {
            if let Some(token_ref) = self.tokens.get(account_id) {
                let token = token_ref.value().clone();
                let upstream = upstream.clone();
                let registry = self.model_registry.clone();
                let cache = cache.clone();
                handles.push(tokio::spawn(async move {
                    let models = fetch_models_for_token(&upstream, &token).await;
                    if !models.is_empty() {
                        let count = models.len();
                        registry.account_models.insert(token.account_id.clone(), models.clone());
                        cache.set_account_models(&token.account_id, models).await;
                        tracing::info!(
                            account_id = %token.account_id,
                            model_count = count,
                            "Refreshed stale models for account"
                        );
                    }
                }));
            }
        }

        for h in handles {
            let _ = h.await;
        }

        self.model_registry.rebuild_aggregated().await;
        cache.save_to_disk();
    }

    /// Load model data from the file cache into the in-memory model registry.
    pub async fn load_models_from_cache(&self, cache: &crate::proxy::model_cache::ModelCache) {
        for entry in cache.account_models().iter() {
            self.model_registry
                .account_models
                .insert(entry.key().clone(), entry.value().clone());
        }
        self.model_registry.rebuild_aggregated().await;
        let count = self.model_registry.all_models.read().await.len();
        tracing::info!(total_models = count, "Model registry loaded from cache");
    }

    /// Return the aggregated (de-duplicated, sorted) list of all models.
    pub async fn get_all_models(&self) -> Vec<String> {
        self.model_registry.all_models.read().await.clone()
    }

    /// Mark an account as rate-limited.
    pub fn mark_rate_limited(
        &self,
        account_id: &str,
        status: u16,
        retry_after: Option<Duration>,
    ) {
        self.rate_limit_tracker
            .mark_limited(account_id, status, retry_after);
    }

    /// Remove a specific model from an account's supported-model set.
    /// Called after a 404 (model not found) from upstream — prevents the same
    /// account from being selected for that model again until the next model
    /// list refresh.
    pub fn remove_model_for_account(&self, account_id: &str, model: &str) {
        self.model_registry
            .remove_model_for_account(account_id, model);
    }

    /// Mark an account request as successful; reset circuit breaker and rate limit.
    pub fn mark_success(&self, account_id: &str) {
        self.circuit_breaker.record_success(account_id);
        self.rate_limit_tracker.clear(account_id);
    }

    /// Mark an account request as failed (HTTP 5xx).
    pub fn mark_failed(&self, account_id: &str) {
        self.circuit_breaker.record_failure(account_id, "upstream_error");
        self.clear_sessions_for_if_tripped(account_id);
    }

    /// Mark a connection-level failure (TCP timeout, DNS error, etc.).
    /// Also applies a rate-limit cooldown.
    pub fn mark_connection_failed(&self, account_id: &str) {
        self.circuit_breaker
            .record_failure(account_id, "connection_failed");
        self.rate_limit_tracker.mark_limited(
            account_id,
            0,
            Some(Duration::from_secs(120)),
        );
        self.clear_sessions_for_if_tripped(account_id);
    }

    /// Mark an account as having a permanent auth failure (401/403).
    /// Trips circuit immediately, applies long rate-limit cooldown.
    pub fn mark_auth_failed(&self, account_id: &str, status: u16) {
        self.circuit_breaker.record_auth_failure(account_id);
        self.rate_limit_tracker.mark_limited(
            account_id,
            status,
            Some(Duration::from_secs(600)),
        );
        self.clear_sessions_for_if_tripped(account_id);
        tracing::error!(
            account_id,
            status,
            "Account auth failed — circuit tripped (will auto-recover after cooldown)"
        );
    }

    /// Clear all sessions pointing at account_id if its circuit is tripped.
    fn clear_sessions_for_if_tripped(&self, account_id: &str) {
        if self.circuit_breaker.is_tripped(account_id) {
            self.session_accounts
                .retain(|_, v| v.account_id.as_str() != account_id);
        }
    }

    /// Remove an account from the pool.
    pub fn remove_account(&self, account_id: &str) {
        self.tokens.remove(account_id);
        self.circuit_breaker.remove(account_id);
        self.rate_limit_tracker.clear(account_id);
        self.session_accounts
            .retain(|_, v| v.account_id.as_str() != account_id);
    }

    /// Set the preferred (fixed) account.
    pub async fn set_preferred_account(&self, id: Option<String>) {
        let mut guard = self.preferred_account_id.write().await;
        *guard = id;
    }

    /// Start the background auto-cleanup task (every 15s).
    /// Also checks for stale model accounts, refreshes them, persists dirty circuit breaker states,
    /// refreshes price cache, and persists proxy stats.
    pub async fn start_auto_cleanup(&self, upstream: Arc<UpstreamClient>) {
        let tracker = self.rate_limit_tracker.clone();
        let cancel = self.cancel_token.child_token();
        let tokens = self.tokens.clone();
        let model_registry = self.model_registry.clone();
        let circuit_breaker = self.circuit_breaker.clone();
        let session_accounts = self.session_accounts.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("Token manager auto-cleanup cancelled");
                        break;
                    }
                    _ = interval.tick() => {
                        tracker.cleanup_expired();

                        // Clean up expired sessions
                        session_accounts.retain(|_, v| v.created_at.elapsed() < SESSION_TTL);

                        // Persist dirty circuit breaker states to config file
                        if circuit_breaker.has_dirty() {
                            let snapshots = circuit_breaker.drain_dirty();
                            if !snapshots.is_empty() {
                                let states: Vec<(String, ProxyHealthState)> = snapshots
                                    .into_iter()
                                    .map(|(id, snap)| {
                                        let disabled = snap.state == "open";
                                        (id, ProxyHealthState {
                                            health_score: if disabled { 0.0 } else { 1.0 },
                                            last_failure_time: std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .map(|d| d.as_secs() as i64)
                                                .unwrap_or(0),
                                            failure_reason: snap.failure_reason,
                                            consecutive_failures: snap.consecutive_failures,
                                            disabled_by_proxy: disabled,
                                            circuit_state: Some(snap.state),
                                            trip_count: snap.trip_count,
                                        })
                                    })
                                    .collect();
                                persist_health_states(&states);
                            }
                        }

                        // Refresh price cache if needed
                        {
                            let price_cache = crate::proxy::price_cache::global();
                            if price_cache.needs_refresh() {
                                let pc = price_cache.clone();
                                tokio::spawn(async move {
                                    pc.refresh().await;
                                });
                            }
                        }

                        // Persist proxy stats if dirty
                        {
                            let stats = crate::proxy::proxy_stats::global();
                            stats.persist_if_dirty();
                        }

                        // Check for stale accounts that need model refresh
                        let cache = crate::proxy::model_cache::global();
                        if cache.has_stale_accounts() {
                            let stale_ids = cache.drain_stale_accounts();
                            if !stale_ids.is_empty() {
                                tracing::info!(count = stale_ids.len(), "Refreshing stale model accounts");
                                let mut handles = Vec::new();
                                for account_id in &stale_ids {
                                    if let Some(token_ref) = tokens.get(account_id) {
                                        let token = token_ref.value().clone();
                                        let upstream = upstream.clone();
                                        let registry = model_registry.clone();
                                        let cache = cache.clone();
                                        handles.push(tokio::spawn(async move {
                                            let models = fetch_models_for_token(&upstream, &token).await;
                                            if !models.is_empty() {
                                                let count = models.len();
                                                registry.account_models.insert(token.account_id.clone(), models.clone());
                                                cache.set_account_models(&token.account_id, models).await;
                                                tracing::info!(
                                                    account_id = %token.account_id,
                                                    model_count = count,
                                                    "Refreshed stale models"
                                                );
                                            }
                                        }));
                                    }
                                }
                                for h in handles {
                                    let _ = h.await;
                                }
                                model_registry.rebuild_aggregated().await;
                                cache.save_to_disk();
                            }
                        }
                    }
                }
            }
        });

        let mut guard = self.auto_cleanup_handle.lock().await;
        if let Some(old) = guard.take() {
            old.abort();
        }
        *guard = Some(handle);
    }

    /// Gracefully shut down the token manager.
    pub async fn graceful_shutdown(&self, timeout: Duration) {
        self.cancel_token.cancel();
        let guard = self.auto_cleanup_handle.lock().await;
        if let Some(handle) = guard.as_ref() {
            let _ = tokio::time::timeout(timeout, async {
                let _ = handle;
            })
            .await;
        }
        // Flush proxy stats on shutdown
        crate::proxy::proxy_stats::global().flush();
    }

    /// Get the number of tokens in the pool.
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// Returns the count of active, healthy (non-tripped, non-rate-limited) accounts,
    /// optionally filtered by protocol compatibility.
    pub fn active_healthy_count(&self, protocol: Option<&str>) -> usize {
        self.tokens
            .iter()
            .filter(|entry| {
                let token = entry.value();
                !self.circuit_breaker.is_tripped(&token.account_id)
                    && !self.rate_limit_tracker.is_limited(&token.account_id)
                    && protocol
                        .map(|p| is_compatible(&token.site_type, p))
                        .unwrap_or(true)
            })
            .count()
    }

    /// Get a reference to the rate limit tracker.
    pub fn rate_limit_tracker(&self) -> &Arc<RateLimitTracker> {
        &self.rate_limit_tracker
    }

    /// Returns true if any accounts have circuit breaker state changes needing persistence.
    pub fn has_dirty_accounts(&self) -> bool {
        self.circuit_breaker.has_dirty()
    }

    /// Get a reference to the circuit breaker registry.
    pub fn circuit_breaker(&self) -> &Arc<CircuitBreakerRegistry> {
        &self.circuit_breaker
    }

    /// Startup preflight check: concurrently verify connectivity for all loaded accounts.
    ///
    /// Sends a lightweight `GET /v1/models` to each account. Based on the response:
    /// - 2xx/3xx/405: healthy (connectable)
    /// - 401/403: `mark_auth_failed` — permanently disabled until user re-enables
    /// - Connection failure: `mark_connection_failed` — 120s cooldown, auto-recovers
    /// - 5xx: logged only, no penalty (transient)
    ///
    /// Runs asynchronously — does not block proxy startup.
    pub async fn preflight_check(&self) {
        let tokens: Vec<ProxyToken> = self.tokens.iter().map(|r| r.value().clone()).collect();

        if tokens.is_empty() {
            return;
        }

        tracing::info!(count = tokens.len(), "Starting preflight check");

        let mut handles = Vec::with_capacity(tokens.len());
        for token in &tokens {
            let account_id = token.account_id.clone();
            let site_url = token.site_url.clone();
            let credential = token.upstream_credential().to_string();

            handles.push(tokio::spawn(async move {
                let url = format!("{}/v1/models", site_url.trim_end_matches('/'));
                let result = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .unwrap_or_default()
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", credential))
                    .send()
                    .await;
                (account_id, result)
            }));
        }

        let mut ok = 0u32;
        let mut auth_failed = 0u32;
        let mut conn_failed = 0u32;

        for handle in handles {
            if let Ok((account_id, result)) = handle.await {
                match result {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        if status == 401 || status == 403 {
                            self.mark_auth_failed(&account_id, status);
                            auth_failed += 1;
                        } else {
                            // 2xx, 3xx, 404, 405, 5xx — all indicate the server is reachable.
                            // 5xx is transient, don't penalize.
                            ok += 1;
                        }
                    }
                    Err(_) => {
                        self.mark_connection_failed(&account_id);
                        conn_failed += 1;
                    }
                }
            }
        }

        tracing::info!(ok, auth_failed, conn_failed, "Preflight check complete");
    }

    /// Drain dirty account IDs and return their current health state for persistence.
    pub fn drain_dirty_health_states(&self) -> Vec<(String, ProxyHealthState)> {
        let snapshots = self.circuit_breaker.drain_dirty();
        snapshots
            .into_iter()
            .map(|(id, snap)| {
                let disabled = snap.state == "open";
                (
                    id,
                    ProxyHealthState {
                        health_score: if disabled { 0.0 } else { 1.0 },
                        last_failure_time: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0),
                        failure_reason: snap.failure_reason,
                        consecutive_failures: snap.consecutive_failures,
                        disabled_by_proxy: disabled,
                        circuit_state: Some(snap.state),
                        trip_count: snap.trip_count,
                    },
                )
            })
            .collect()
    }
}

/// Result from a model fetch attempt.
enum FetchResult {
    /// Successfully fetched models.
    Ok(HashSet<String>),
    /// Auth failure (401/403) — caller should stop retrying this account.
    AuthFailed,
    /// Other failure (network, 5xx, etc.) — empty set, but not necessarily permanent.
    Empty,
}

/// Fetch models for a single ProxyToken using the correct endpoint for its site type.
async fn fetch_models_for_token(
    upstream: &UpstreamClient,
    token: &ProxyToken,
) -> HashSet<String> {
    match token.site_type.as_str() {
        "sub2api" => HashSet::new(),
        "one-hub" | "done-hub" => {
            match fetch_models_via_endpoint(upstream, token, "/api/available_model", parse_onehub_models).await {
                FetchResult::Ok(models) => models,
                FetchResult::AuthFailed | FetchResult::Empty => HashSet::new(),
            }
        }
        _ => {
            // New-API family: try /api/user/models first, fallback to /v1/models
            match fetch_models_via_endpoint(
                upstream, token, "/api/user/models", parse_newapi_models,
            ).await {
                FetchResult::Ok(models) => return models,
                FetchResult::AuthFailed => return HashSet::new(),
                FetchResult::Empty => {
                    // Not auth failure — try fallback endpoint
                }
            }
            match fetch_models_via_endpoint(upstream, token, "/v1/models", parse_openai_models).await {
                FetchResult::Ok(models) => models,
                FetchResult::AuthFailed | FetchResult::Empty => HashSet::new(),
            }
        }
    }
}

async fn fetch_models_via_endpoint(
    upstream: &UpstreamClient,
    token: &ProxyToken,
    path: &str,
    parser: fn(&serde_json::Value) -> HashSet<String>,
) -> FetchResult {
    let resp = upstream
        .forward(
            &token.site_url,
            path,
            reqwest::Method::GET,
            reqwest::header::HeaderMap::new(),
            bytes::Bytes::new(),
            &token.access_token,
        )
        .await;

    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            if status >= 200 && status < 300 {
                if let Ok(body) = r.json::<serde_json::Value>().await {
                    let models = parser(&body);
                    if !models.is_empty() {
                        return FetchResult::Ok(models);
                    }
                }
                return FetchResult::Empty;
            }
            if status == 401 || status == 403 {
                tracing::warn!(
                    account_id = %token.account_id,
                    path,
                    status,
                    "Auth failed during model fetch"
                );
                return FetchResult::AuthFailed;
            }
            FetchResult::Empty
        }
        Err(e) => {
            tracing::warn!(
                account_id = %token.account_id,
                path,
                error = %e,
                "Failed to connect for model fetch"
            );
            FetchResult::Empty
        }
    }
}

/// Parse /api/user/models response: { "data": ["model-a", "model-b"] }
fn parse_newapi_models(body: &serde_json::Value) -> HashSet<String> {
    body.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Parse /api/available_model response: { "data": { "model-name": {...} } }
fn parse_onehub_models(body: &serde_json::Value) -> HashSet<String> {
    body.get("data")
        .and_then(|d| d.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

/// Parse /v1/models response: { "data": [{"id": "model-name"}, ...] }
fn parse_openai_models(body: &serde_json::Value) -> HashSet<String> {
    body.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("id").and_then(|id| id.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Check whether a `site_type` is compatible with the requested API `protocol`.
///
/// Uses negative matching (exclude known-bad) so that new site types default to compatible.
fn is_compatible(site_type: &str, protocol: &str) -> bool {
    match protocol {
        "anthropic" => !matches!(site_type, "one-hub" | "done-hub" | "sub2api"),
        "gemini" => !matches!(site_type, "one-hub" | "done-hub" | "sub2api" | "one-api"),
        "openai" => site_type != "sub2api",
        _ => true,
    }
}

/// Persist health states to the config file.
/// Reads the current config, updates the matching proxy_accounts, and writes back.
fn persist_health_states(states: &[(String, ProxyHealthState)]) {
    let mut config = crate::modules::config::load_app_config();
    let mut updated = 0usize;

    for (account_id, health_state) in states {
        for account in &mut config.proxy_accounts {
            if account.id == *account_id {
                account.proxy_health = Some(health_state.clone());
                updated += 1;
            }
        }
        // Also update in the main accounts list for consistency
        for account in &mut config.accounts {
            if account.id == *account_id {
                account.proxy_health = Some(health_state.clone());
            }
        }
    }

    if updated > 0 {
        if let Err(e) = crate::modules::config::save_app_config(&config) {
            tracing::error!(error = %e, "Failed to persist health states to config");
        } else {
            tracing::info!(
                updated_accounts = updated,
                "Persisted health states to config"
            );
        }
    }
}

impl Default for TokenManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use crate::models::{AccountInfo, SiteAccount};

    fn make_account(id: &str, disabled: bool) -> SiteAccount {
        SiteAccount {
            id: id.to_string(),
            site_name: format!("Site {}", id),
            site_url: format!("https://{}.example.com", id),
            site_type: "new-api".to_string(),
            account_info: AccountInfo {
                id: 1,
                access_token: format!("sk-{}", id),
                api_key: Some(format!("sk-api-{}", id)),
                username: format!("user_{}", id),
                quota: 1000.0,
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
            disabled: Some(disabled),
            health: None,
            exchange_rate: None,
            proxy_health: None,
            proxy_priority: 0,
            proxy_weight: 10,
        }
    }

    fn make_account_with_type(id: &str, disabled: bool, site_type: &str) -> SiteAccount {
        let mut account = make_account(id, disabled);
        account.site_type = site_type.to_string();
        account
    }

    #[test]
    fn token_manager_round_robin() {
        let tm = TokenManager::new();
        let accounts = vec![
            make_account("a", false),
            make_account("b", false),
            make_account("c", false),
        ];
        tm.load_from_accounts(&accounts);
        assert_eq!(tm.token_count(), 3);

        // Get 3 tokens — should cycle through all
        let mut ids: Vec<String> = Vec::new();
        for _ in 0..3 {
            let t = tm.get_token(None, None, None).unwrap();
            ids.push(t.account_id.clone());
        }
        // All three should be present (order may vary due to DashMap iteration)
        ids.sort();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn token_manager_skip_disabled() {
        let tm = TokenManager::new();
        let accounts = vec![
            make_account("a", true), // disabled
            make_account("b", false),
        ];
        tm.load_from_accounts(&accounts);
        // Only "b" should be in the pool (disabled filtered during load)
        assert_eq!(tm.token_count(), 1);

        let t = tm.get_token(None, None, None).unwrap();
        assert_eq!(t.account_id, "b");
    }

    #[test]
    fn token_manager_skip_rate_limited() {
        let tm = TokenManager::new();
        let accounts = vec![make_account("a", false), make_account("b", false)];
        tm.load_from_accounts(&accounts);

        tm.mark_rate_limited("a", 429, Some(Duration::from_secs(300)));

        // Should always return "b" since "a" is rate-limited
        for _ in 0..5 {
            let t = tm.get_token(None, None, None).unwrap();
            assert_eq!(t.account_id, "b");
        }
    }

    #[tokio::test]
    async fn token_manager_preferred() {
        let tm = TokenManager::new();
        let accounts = vec![make_account("a", false), make_account("b", false)];
        tm.load_from_accounts(&accounts);

        tm.set_preferred_account(Some("b".to_string())).await;

        // Should always return "b"
        for _ in 0..5 {
            let t = tm.get_token(None, None, None).unwrap();
            assert_eq!(t.account_id, "b");
        }
    }

    #[test]
    fn token_manager_empty_pool() {
        let tm = TokenManager::new();
        assert!(tm.get_token(None, None, None).is_none());
    }

    #[test]
    fn token_manager_model_filter() {
        let tm = TokenManager::new();
        let accounts = vec![make_account("a", false), make_account("b", false)];
        tm.load_from_accounts(&accounts);

        // Populate model registry: "a" has gpt-4, "b" has claude-3
        let mut set_a = HashSet::new();
        set_a.insert("gpt-4".to_string());
        tm.model_registry.account_models.insert("a".to_string(), set_a);

        let mut set_b = HashSet::new();
        set_b.insert("claude-3".to_string());
        tm.model_registry.account_models.insert("b".to_string(), set_b);

        // Requesting gpt-4 should only return "a"
        for _ in 0..5 {
            let t = tm.get_token(None, Some("gpt-4"), None).unwrap();
            assert_eq!(t.account_id, "a");
        }

        // Requesting claude-3 should only return "b"
        for _ in 0..5 {
            let t = tm.get_token(None, Some("claude-3"), None).unwrap();
            assert_eq!(t.account_id, "b");
        }

        // Requesting unknown model should return None (both accounts have registry data)
        assert!(tm.get_token(None, Some("nonexistent-model"), None).is_none());
    }

    #[test]
    fn mark_auth_failed_excludes_account() {
        let tm = TokenManager::new();
        let accounts = vec![make_account("a", false), make_account("b", false)];
        tm.load_from_accounts(&accounts);

        // Auth failure should immediately trip the circuit
        tm.mark_auth_failed("a", 401);

        // Circuit should be tripped
        assert!(tm.circuit_breaker.is_tripped("a"));

        // Should also be rate-limited (10 min cooldown)
        assert!(tm.rate_limit_tracker.is_limited("a"));

        // Only "b" should be returned
        for _ in 0..5 {
            let t = tm.get_token(None, None, None).unwrap();
            assert_eq!(t.account_id, "b");
        }
    }

    #[test]
    fn health_recovery_via_circuit_breaker() {
        // Circuit breaker with very short cooldown for testing
        let tm = TokenManager::with_mode(LoadBalanceMode::RoundRobin);

        let accounts = vec![make_account("a", false)];
        tm.load_from_accounts(&accounts);

        // Trip the circuit (5 failures)
        for _ in 0..5 {
            tm.mark_failed("a");
        }
        assert!(tm.circuit_breaker.is_tripped("a"));
        assert!(tm.get_token(None, None, None).is_none());
    }

    #[test]
    fn protocol_filter_anthropic() {
        let tm = TokenManager::new();
        let accounts = vec![
            make_account_with_type("newapi", false, "new-api"),
            make_account_with_type("donehub", false, "done-hub"),
            make_account_with_type("sub2api", false, "sub2api"),
        ];
        tm.load_from_accounts(&accounts);
        assert_eq!(tm.token_count(), 3);

        // Anthropic protocol should only return new-api, never done-hub or sub2api
        for _ in 0..10 {
            let t = tm.get_token(None, None, Some("anthropic")).unwrap();
            assert_eq!(t.account_id, "newapi");
        }

        // OpenAI protocol should return new-api or done-hub, never sub2api
        let mut seen = HashSet::new();
        for _ in 0..20 {
            let t = tm.get_token(None, None, Some("openai")).unwrap();
            assert_ne!(t.account_id, "sub2api");
            seen.insert(t.account_id.clone());
        }
        assert!(seen.contains("newapi"));
        assert!(seen.contains("donehub"));

        // No protocol filter → all three are candidates
        let mut seen_all = HashSet::new();
        for _ in 0..30 {
            let t = tm.get_token(None, None, None).unwrap();
            seen_all.insert(t.account_id.clone());
        }
        assert_eq!(seen_all.len(), 3);
    }

    #[test]
    fn protocol_filter_gemini() {
        let tm = TokenManager::new();
        let accounts = vec![
            make_account_with_type("newapi", false, "new-api"),
            make_account_with_type("oneapi", false, "one-api"),
            make_account_with_type("onehub", false, "one-hub"),
        ];
        tm.load_from_accounts(&accounts);

        // Gemini protocol excludes one-api, one-hub
        for _ in 0..10 {
            let t = tm.get_token(None, None, Some("gemini")).unwrap();
            assert_eq!(t.account_id, "newapi");
        }
    }

    #[test]
    fn consecutive_failures_auto_disable() {
        let tm = TokenManager::new();
        let accounts = vec![make_account("a", false), make_account("b", false)];
        tm.load_from_accounts(&accounts);

        // 5 consecutive failures should trip the circuit
        for _ in 0..5 {
            tm.mark_failed("a");
        }

        // Account "a" should be tripped
        assert!(tm.circuit_breaker.is_tripped("a"));

        // Only "b" should be returned
        for _ in 0..5 {
            let t = tm.get_token(None, None, None).unwrap();
            assert_eq!(t.account_id, "b");
        }
    }

    #[test]
    fn connection_failed_applies_rate_limit() {
        let tm = TokenManager::new();
        let accounts = vec![make_account("a", false), make_account("b", false)];
        tm.load_from_accounts(&accounts);

        tm.mark_connection_failed("a");

        // Should be rate-limited (120s cooldown)
        assert!(tm.rate_limit_tracker.is_limited("a"));

        // Only "b" should be returned
        for _ in 0..5 {
            let t = tm.get_token(None, None, None).unwrap();
            assert_eq!(t.account_id, "b");
        }
    }

    #[test]
    fn get_token_excluding_skips_listed() {
        let tm = TokenManager::new();
        let accounts = vec![make_account("a", false), make_account("b", false)];
        tm.load_from_accounts(&accounts);

        // Exclude "a" — should always get "b"
        for _ in 0..5 {
            let t = tm
                .get_token_excluding(None, None, None, &["a".to_string()])
                .unwrap();
            assert_eq!(t.account_id, "b");
        }

        // Exclude both — should get None
        assert!(tm
            .get_token_excluding(
                None,
                None,
                None,
                &["a".to_string(), "b".to_string()]
            )
            .is_none());
    }

    #[test]
    fn success_resets_consecutive_failures() {
        let tm = TokenManager::new();
        let accounts = vec![make_account("a", false)];
        tm.load_from_accounts(&accounts);

        // 3 failures (not enough to trip)
        for _ in 0..3 {
            tm.mark_failed("a");
        }
        let (_, failures, _) = tm.circuit_breaker.get_state("a").unwrap();
        assert_eq!(failures, 3);

        // Success resets
        tm.mark_success("a");
        let (state, failures, trips) = tm.circuit_breaker.get_state("a").unwrap();
        assert_eq!(state, crate::proxy::circuit_breaker::CircuitState::Closed);
        assert_eq!(failures, 0);
        assert_eq!(trips, 0);
    }

    #[test]
    fn load_initializes_circuit_from_proxy_health() {
        let mut account = make_account("a", false);
        account.proxy_health = Some(crate::models::ProxyHealthState {
            health_score: 0.0,
            last_failure_time: 1000,
            failure_reason: "auth_failed".to_string(),
            consecutive_failures: 5,
            disabled_by_proxy: true,
            circuit_state: Some("open".to_string()),
            trip_count: 2,
        });

        let tm = TokenManager::new();
        tm.load_from_accounts(&[account, make_account("b", false)]);

        // Both accounts should be loaded (no longer skips disabled_by_proxy)
        assert_eq!(tm.token_count(), 2);
        // "a" should be tripped
        assert!(tm.circuit_breaker.is_tripped("a"));
    }

    #[test]
    fn load_migrates_old_disabled_by_proxy() {
        let mut account = make_account("a", false);
        account.proxy_health = Some(crate::models::ProxyHealthState {
            health_score: 0.0,
            last_failure_time: 1000,
            failure_reason: "auth_failed".to_string(),
            consecutive_failures: 5,
            disabled_by_proxy: true,
            circuit_state: None, // old format
            trip_count: 0,
        });

        let tm = TokenManager::new();
        tm.load_from_accounts(&[account]);

        // Should be loaded and tripped via migration
        assert_eq!(tm.token_count(), 1);
        assert!(tm.circuit_breaker.is_tripped("a"));
    }

    #[test]
    fn remove_model_for_account_prevents_routing() {
        let tm = TokenManager::new();
        let accounts = vec![make_account("a", false), make_account("b", false)];
        tm.load_from_accounts(&accounts);

        // Both accounts have model registries
        let mut set_a = HashSet::new();
        set_a.insert("claude-opus".to_string());
        set_a.insert("claude-haiku".to_string());
        tm.model_registry
            .account_models
            .insert("a".to_string(), set_a);

        let mut set_b = HashSet::new();
        set_b.insert("claude-haiku".to_string());
        // "b" does NOT have claude-opus
        tm.model_registry
            .account_models
            .insert("b".to_string(), set_b);

        // Before removal: only "a" supports claude-opus
        for _ in 0..5 {
            let t = tm.get_token(None, Some("claude-opus"), None).unwrap();
            assert_eq!(t.account_id, "a");
        }

        // Simulate 404: remove claude-opus from account "a"
        tm.remove_model_for_account("a", "claude-opus");

        // Now no account supports claude-opus
        assert!(tm.get_token(None, Some("claude-opus"), None).is_none());

        // claude-haiku should still work for both accounts
        let t = tm.get_token(None, Some("claude-haiku"), None).unwrap();
        assert!(t.account_id == "a" || t.account_id == "b");
    }

    #[test]
    fn remove_model_noop_for_unknown_account() {
        let tm = TokenManager::new();
        let accounts = vec![make_account("a", false)];
        tm.load_from_accounts(&accounts);

        // No registry data for "a" — remove should be a no-op (no panic)
        tm.remove_model_for_account("a", "some-model");

        // Account still works for any model (no registry = allow all)
        assert!(tm.get_token(None, Some("some-model"), None).is_some());
    }
}
