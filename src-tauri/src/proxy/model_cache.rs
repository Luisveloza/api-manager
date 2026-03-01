//! File-persisted model cache.
//!
//! Models rarely change, so we fetch once, persist to disk, and reuse across
//! restarts.  When a proxy handler receives a 404 (model not found), it marks
//! the originating account as *stale*; a background task later re-fetches only
//! those accounts.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};

use crate::modules::config::get_data_dir;

const CACHE_FILE_NAME: &str = "model_cache.json";

/// Minimum interval between stale marks for the same account (5 minutes).
const STALE_COOLDOWN_SECS: u64 = 300;

// ---------------------------------------------------------------------------
// Persisted format
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Default)]
struct CacheFile {
    /// account_id -> list of model ids
    per_account: HashMap<String, Vec<String>>,
}

// ---------------------------------------------------------------------------
// Runtime cache
// ---------------------------------------------------------------------------

pub struct ModelCache {
    /// account_id -> set of model ids
    per_account: DashMap<String, HashSet<String>>,
    /// Aggregated sorted model list (rebuilt after mutations).
    all_models: RwLock<Vec<String>>,
    /// Account ids whose model lists need refreshing.
    stale_accounts: DashMap<String, ()>,
    /// Cooldown: last time each account was marked stale (to prevent spam).
    stale_cooldown: DashMap<String, Instant>,
    /// Guard: only one upstream fetch task at a time.
    fetch_guard: Mutex<()>,
}

impl ModelCache {
    fn new() -> Self {
        Self {
            per_account: DashMap::new(),
            all_models: RwLock::new(Vec::new()),
            stale_accounts: DashMap::new(),
            stale_cooldown: DashMap::new(),
            fetch_guard: Mutex::new(()),
        }
    }

    // -- Queries ------------------------------------------------------------

    /// Returns the aggregated, sorted model list.
    pub async fn get_all_models(&self) -> Vec<String> {
        self.all_models.read().await.clone()
    }

    /// Returns true when no per-account data is loaded.
    pub fn is_empty(&self) -> bool {
        self.per_account.is_empty()
    }

    /// Per-account lookup used by TokenManager routing.
    pub fn account_models(&self) -> &DashMap<String, HashSet<String>> {
        &self.per_account
    }

    /// Returns the set of account ids marked as stale, draining them.
    pub fn drain_stale_accounts(&self) -> Vec<String> {
        let ids: Vec<String> = self.stale_accounts.iter().map(|r| r.key().clone()).collect();
        for id in &ids {
            self.stale_accounts.remove(id);
        }
        ids
    }

    pub fn has_stale_accounts(&self) -> bool {
        !self.stale_accounts.is_empty()
    }

    // -- Mutations ----------------------------------------------------------

    /// Mark an account's model list as stale (e.g. after a 404 from upstream).
    /// Respects a cooldown period to avoid spamming refreshes when the model
    /// genuinely doesn't exist on the upstream.
    pub fn mark_stale(&self, account_id: &str) {
        // Already pending refresh
        if self.stale_accounts.contains_key(account_id) {
            return;
        }
        // Cooldown check: don't re-mark within STALE_COOLDOWN_SECS
        if let Some(last) = self.stale_cooldown.get(account_id) {
            if last.elapsed().as_secs() < STALE_COOLDOWN_SECS {
                return;
            }
        }
        self.stale_cooldown.insert(account_id.to_string(), Instant::now());
        self.stale_accounts.insert(account_id.to_string(), ());
        tracing::info!(account_id, "Model list marked stale — will refresh");
    }

    /// Insert models for a single account and rebuild the aggregate list.
    pub async fn set_account_models(&self, account_id: &str, models: HashSet<String>) {
        if models.is_empty() {
            return;
        }
        self.per_account.insert(account_id.to_string(), models);
        self.rebuild_aggregated().await;
    }

    /// Bulk-load per-account data (typically from file cache).
    pub async fn load_bulk(&self, data: HashMap<String, HashSet<String>>) {
        self.per_account.clear();
        for (id, models) in data {
            self.per_account.insert(id, models);
        }
        self.rebuild_aggregated().await;
    }

    /// Acquire the fetch guard — returns a guard that must be held while
    /// fetching from upstreams.  If the lock is already held, the caller
    /// should skip fetching and wait for the result instead.
    pub fn try_acquire_fetch_guard(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        self.fetch_guard.try_lock().ok()
    }

    // -- Persistence --------------------------------------------------------

    /// Persist current state to disk.
    pub fn save_to_disk(&self) {
        let path = match cache_file_path() {
            Some(p) => p,
            None => return,
        };

        let mut data = HashMap::new();
        for entry in self.per_account.iter() {
            let models: Vec<String> = entry.value().iter().cloned().collect();
            data.insert(entry.key().clone(), models);
        }

        let file = CacheFile { per_account: data };
        match serde_json::to_string_pretty(&file) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!(error = %e, "Failed to write model cache");
                } else {
                    tracing::info!(?path, "Model cache persisted to disk");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to serialize model cache");
            }
        }
    }

    /// Load from disk into memory, rebuilding the aggregate list.
    pub async fn load_from_disk(&self) {
        let path = match cache_file_path() {
            Some(p) => p,
            None => return,
        };

        if !path.exists() {
            tracing::info!("No model cache file found — will fetch from upstreams");
            return;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read model cache file");
                return;
            }
        };

        let file: CacheFile = match serde_json::from_str(&content) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to parse model cache file");
                return;
            }
        };

        let mut count = 0usize;
        for (id, models) in file.per_account {
            let set: HashSet<String> = models.into_iter().collect();
            count += set.len();
            self.per_account.insert(id, set);
        }

        self.rebuild_aggregated().await;

        let total = self.all_models.read().await.len();
        tracing::info!(
            accounts = self.per_account.len(),
            total_per_account_entries = count,
            unique_models = total,
            "Model cache loaded from disk"
        );
    }

    // -- Internal -----------------------------------------------------------

    async fn rebuild_aggregated(&self) {
        let mut merged = HashSet::new();
        for entry in self.per_account.iter() {
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

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

static GLOBAL_CACHE: OnceLock<Arc<ModelCache>> = OnceLock::new();

/// Get (or create) the global model cache instance.
pub fn global() -> Arc<ModelCache> {
    GLOBAL_CACHE
        .get_or_init(|| Arc::new(ModelCache::new()))
        .clone()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cache_file_path() -> Option<PathBuf> {
    get_data_dir().ok().map(|d| d.join(CACHE_FILE_NAME))
}
