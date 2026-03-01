//! Aggregated proxy statistics — per-account and global totals.
//!
//! Accumulated in memory, periodically persisted to `data/proxy_stats.json`.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use crate::modules::config::get_data_dir;
use crate::proxy::monitor::ProxyRequestLog;

const STATS_FILE_NAME: &str = "proxy_stats.json";

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountStats {
    pub total_requests: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_estimated_cost: f64,
    pub total_duration_ms: u64,
}

impl AccountStats {
    pub fn success_rate(&self) -> f64 {
        if self.total_requests == 0 {
            return 0.0;
        }
        self.success_count as f64 / self.total_requests as f64
    }

    pub fn avg_latency_ms(&self) -> f64 {
        if self.total_requests == 0 {
            return 0.0;
        }
        self.total_duration_ms as f64 / self.total_requests as f64
    }
}

/// Hourly bucket for timeline stats.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HourlyBucket {
    /// Unix timestamp aligned to the start of the hour.
    pub hour: i64,
    pub total_requests: u64,
    pub success_count: u64,
    pub total_tokens: u64,
    pub total_cost: f64,
}

/// Per-key cost statistics for multi-user API key tracking.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PerKeyStats {
    pub total_requests: u64,
    pub total_cost: f64,
    /// Cost accumulated today (UTC).
    pub today_cost: f64,
    /// The date (as "YYYY-MM-DD") for `today_cost`. Rolls over at midnight UTC.
    #[serde(default)]
    pub today_date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyStatsData {
    pub per_account: HashMap<String, AccountStats>,
    pub global: AccountStats,
    /// Per-model aggregated stats.
    #[serde(default)]
    pub per_model: HashMap<String, AccountStats>,
    /// Last 24 hours of hourly buckets.
    #[serde(default)]
    pub hourly_buckets: VecDeque<HourlyBucket>,
    /// Per-API-key cost tracking for multi-user management.
    #[serde(default)]
    pub per_key: HashMap<String, PerKeyStats>,
}

// ---------------------------------------------------------------------------
// Accumulator
// ---------------------------------------------------------------------------

pub struct StatsAccumulator {
    data: RwLock<ProxyStatsData>,
    dirty: AtomicBool,
}

impl StatsAccumulator {
    fn new() -> Self {
        Self {
            data: RwLock::new(ProxyStatsData::default()),
            dirty: AtomicBool::new(false),
        }
    }

    /// Record a request log entry into stats.
    pub fn record(&self, log: &ProxyRequestLog) {
        let mut data = match self.data.write() {
            Ok(d) => d,
            Err(_) => return,
        };

        let is_success = log.status >= 200 && log.status < 400;
        let input = log.input_tokens.unwrap_or(0) as i64;
        let output = log.output_tokens.unwrap_or(0) as i64;
        let cost = log.estimated_cost.unwrap_or(0.0);

        // Update global
        data.global.total_requests += 1;
        if is_success {
            data.global.success_count += 1;
        } else {
            data.global.error_count += 1;
        }
        data.global.total_input_tokens += input;
        data.global.total_output_tokens += output;
        data.global.total_estimated_cost += cost;
        data.global.total_duration_ms += log.duration_ms;

        // Update per-account
        if let Some(ref account_id) = log.account_id {
            let entry = data
                .per_account
                .entry(account_id.clone())
                .or_default();
            entry.total_requests += 1;
            if is_success {
                entry.success_count += 1;
            } else {
                entry.error_count += 1;
            }
            entry.total_input_tokens += input;
            entry.total_output_tokens += output;
            entry.total_estimated_cost += cost;
            entry.total_duration_ms += log.duration_ms;
        }

        // Update per-model
        if let Some(ref model) = log.model {
            if !model.is_empty() {
                let entry = data.per_model.entry(model.clone()).or_default();
                entry.total_requests += 1;
                if is_success {
                    entry.success_count += 1;
                } else {
                    entry.error_count += 1;
                }
                entry.total_input_tokens += input;
                entry.total_output_tokens += output;
                entry.total_estimated_cost += cost;
                entry.total_duration_ms += log.duration_ms;
            }
        }

        // Update hourly buckets
        let hour_ts = (log.timestamp / 3600) * 3600;
        let total_tokens = (input + output).max(0) as u64;
        match data.hourly_buckets.back_mut() {
            Some(bucket) if bucket.hour == hour_ts => {
                bucket.total_requests += 1;
                if is_success {
                    bucket.success_count += 1;
                }
                bucket.total_tokens += total_tokens;
                bucket.total_cost += cost;
            }
            _ => {
                data.hourly_buckets.push_back(HourlyBucket {
                    hour: hour_ts,
                    total_requests: 1,
                    success_count: if is_success { 1 } else { 0 },
                    total_tokens,
                    total_cost: cost,
                });
                // Keep only last 24 hours
                while data.hourly_buckets.len() > 24 {
                    data.hourly_buckets.pop_front();
                }
            }
        }

        // Update per-key stats
        if let Some(ref key) = log.api_key {
            let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let entry = data.per_key.entry(key.clone()).or_default();
            entry.total_requests += 1;
            entry.total_cost += cost;
            // Roll over today_cost if date changed
            if entry.today_date != today {
                entry.today_cost = 0.0;
                entry.today_date = today;
            }
            entry.today_cost += cost;
        }

        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Get a snapshot of current stats.
    pub fn get_stats(&self) -> ProxyStatsData {
        self.data.read().map(|d| d.clone()).unwrap_or_default()
    }

    /// Get cost stats for a specific API key.
    pub fn get_per_key_stats(&self, key: &str) -> PerKeyStats {
        self.data
            .read()
            .ok()
            .and_then(|d| d.per_key.get(key).cloned())
            .unwrap_or_default()
    }

    /// Get all per-key stats.
    pub fn all_key_stats(&self) -> HashMap<String, PerKeyStats> {
        self.data
            .read()
            .map(|d| d.per_key.clone())
            .unwrap_or_default()
    }

    /// Get per-model stats sorted by request count desc, top N.
    pub fn stats_by_model(&self, limit: usize) -> Vec<(String, AccountStats)> {
        let data = match self.data.read() {
            Ok(d) => d,
            Err(_) => return vec![],
        };
        let mut items: Vec<(String, AccountStats)> = data
            .per_model
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        items.sort_by(|a, b| b.1.total_requests.cmp(&a.1.total_requests));
        items.truncate(limit);
        items
    }

    /// Get last 24h hourly timeline.
    pub fn stats_timeline(&self) -> VecDeque<HourlyBucket> {
        self.data
            .read()
            .map(|d| d.hourly_buckets.clone())
            .unwrap_or_default()
    }

    /// Get today's total estimated cost (based on hourly buckets for the current day).
    pub fn today_total_cost(&self) -> f64 {
        let data = match self.data.read() {
            Ok(d) => d,
            Err(_) => return 0.0,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        // Start of today (UTC)
        let today_start = (now / 86400) * 86400;
        data.hourly_buckets
            .iter()
            .filter(|b| b.hour >= today_start)
            .map(|b| b.total_cost)
            .sum()
    }

    /// Persist to disk if dirty.
    pub fn persist_if_dirty(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        self.write_to_disk();
    }

    /// Force flush to disk.
    pub fn flush(&self) {
        self.dirty.store(false, Ordering::Relaxed);
        self.write_to_disk();
    }

    /// Load stats from disk (called at startup).
    pub fn load_from_disk(&self) {
        let path = match stats_file_path() {
            Some(p) => p,
            None => return,
        };

        if !path.exists() {
            return;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read proxy stats file");
                return;
            }
        };

        let loaded: ProxyStatsData = match serde_json::from_str(&content) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to parse proxy stats file");
                return;
            }
        };

        if let Ok(mut data) = self.data.write() {
            *data = loaded;
        }
        tracing::info!("Proxy stats loaded from disk");
    }

    fn write_to_disk(&self) {
        let path = match stats_file_path() {
            Some(p) => p,
            None => return,
        };

        let data = match self.data.read() {
            Ok(d) => d.clone(),
            Err(_) => return,
        };

        match serde_json::to_string_pretty(&data) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!(error = %e, "Failed to write proxy stats");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to serialize proxy stats");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

static GLOBAL_STATS: OnceLock<Arc<StatsAccumulator>> = OnceLock::new();

pub fn global() -> Arc<StatsAccumulator> {
    GLOBAL_STATS
        .get_or_init(|| Arc::new(StatsAccumulator::new()))
        .clone()
}

fn stats_file_path() -> Option<PathBuf> {
    get_data_dir().ok().map(|d| d.join(STATS_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_log(account_id: &str, status: u16) -> ProxyRequestLog {
        ProxyRequestLog {
            id: "test".to_string(),
            timestamp: 0,
            method: "POST".to_string(),
            url: "/v1/chat/completions".to_string(),
            status,
            duration_ms: 100,
            model: Some("gpt-4".to_string()),
            account_id: Some(account_id.to_string()),
            upstream_url: None,
            client_ip: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            error: None,
            estimated_cost: Some(0.005),
            request_body: None,
            response_body: None,
            api_key: None,
        }
    }

    #[test]
    fn record_accumulates() {
        let acc = StatsAccumulator::new();
        acc.record(&make_log("a", 200));
        acc.record(&make_log("a", 200));
        acc.record(&make_log("b", 500));

        let stats = acc.get_stats();
        assert_eq!(stats.global.total_requests, 3);
        assert_eq!(stats.global.success_count, 2);
        assert_eq!(stats.global.error_count, 1);
        assert_eq!(stats.per_account["a"].total_requests, 2);
        assert_eq!(stats.per_account["b"].total_requests, 1);
    }

    #[test]
    fn success_rate_calculation() {
        let stats = AccountStats {
            total_requests: 10,
            success_count: 8,
            error_count: 2,
            ..Default::default()
        };
        assert!((stats.success_rate() - 0.8).abs() < 1e-10);
    }

    #[test]
    fn avg_latency_calculation() {
        let stats = AccountStats {
            total_requests: 4,
            total_duration_ms: 400,
            ..Default::default()
        };
        assert!((stats.avg_latency_ms() - 100.0).abs() < 1e-10);
    }
}
