use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// A single proxy request log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRequestLog {
    pub id: String,
    pub timestamp: i64,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub duration_ms: u64,
    pub model: Option<String>,
    pub account_id: Option<String>,
    pub upstream_url: Option<String>,
    pub client_ip: Option<String>,
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub error: Option<String>,
    /// Estimated cost in USD based on model pricing data. None if model not in price cache.
    #[serde(default)]
    pub estimated_cost: Option<f64>,
    /// Truncated request body (up to 4KB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    /// Truncated response body (up to 4KB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    /// API key identifier (key string) if request was authenticated via a user API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// In-memory ring buffer of proxy request logs.
pub struct ProxyMonitor {
    logs: RwLock<VecDeque<ProxyRequestLog>>,
    enabled: AtomicBool,
    max_logs: usize,
}

impl ProxyMonitor {
    pub fn new(max_logs: usize) -> Self {
        Self {
            logs: RwLock::new(VecDeque::with_capacity(max_logs)),
            enabled: AtomicBool::new(true),
            max_logs,
        }
    }

    /// Add a log entry. If capacity is exceeded, oldest entry is dropped.
    pub fn add_log(&self, log: ProxyRequestLog) {
        if !self.is_enabled() {
            return;
        }
        if let Ok(mut logs) = self.logs.write() {
            if logs.len() >= self.max_logs {
                logs.pop_front();
            }
            logs.push_back(log);
        }
    }

    /// Get logs with pagination (offset + limit).
    pub fn get_logs(&self, offset: usize, limit: usize) -> Vec<ProxyRequestLog> {
        if let Ok(logs) = self.logs.read() {
            // Return in reverse order (newest first)
            let total = logs.len();
            if offset >= total {
                return Vec::new();
            }
            logs.iter()
                .rev()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Get a specific log by id.
    pub fn get_log(&self, id: &str) -> Option<ProxyRequestLog> {
        if let Ok(logs) = self.logs.read() {
            logs.iter().find(|l| l.id == id).cloned()
        } else {
            None
        }
    }

    /// Get total log count.
    pub fn get_count(&self) -> usize {
        self.logs.read().map(|l| l.len()).unwrap_or(0)
    }

    /// Clear all logs.
    pub fn clear(&self) {
        if let Ok(mut logs) = self.logs.write() {
            logs.clear();
        }
    }

    /// Enable or disable logging.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Check if logging is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

impl Default for ProxyMonitor {
    fn default() -> Self {
        Self::new(1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_log(id: &str, status: u16) -> ProxyRequestLog {
        ProxyRequestLog {
            id: id.to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            method: "POST".to_string(),
            url: "/v1/chat/completions".to_string(),
            status,
            duration_ms: 150,
            model: Some("gpt-4".to_string()),
            account_id: Some("acc-1".to_string()),
            upstream_url: Some("https://api.example.com".to_string()),
            client_ip: Some("127.0.0.1".to_string()),
            input_tokens: Some(100),
            output_tokens: Some(50),
            error: None,
            estimated_cost: None,
            request_body: None,
            response_body: None,
            api_key: None,
        }
    }

    #[test]
    fn monitor_add_and_get() {
        let monitor = ProxyMonitor::new(100);
        monitor.add_log(make_log("log-1", 200));
        monitor.add_log(make_log("log-2", 200));

        assert_eq!(monitor.get_count(), 2);

        let logs = monitor.get_logs(0, 10);
        assert_eq!(logs.len(), 2);
        // Newest first
        assert_eq!(logs[0].id, "log-2");
        assert_eq!(logs[1].id, "log-1");
    }

    #[test]
    fn monitor_ring_buffer_eviction() {
        let monitor = ProxyMonitor::new(3);
        monitor.add_log(make_log("log-1", 200));
        monitor.add_log(make_log("log-2", 200));
        monitor.add_log(make_log("log-3", 200));
        monitor.add_log(make_log("log-4", 200)); // evicts log-1

        assert_eq!(monitor.get_count(), 3);
        assert!(monitor.get_log("log-1").is_none());
        assert!(monitor.get_log("log-4").is_some());
    }

    #[test]
    fn monitor_disabled_skips_add() {
        let monitor = ProxyMonitor::new(100);
        monitor.set_enabled(false);
        monitor.add_log(make_log("log-1", 200));
        assert_eq!(monitor.get_count(), 0);
    }

    #[test]
    fn monitor_pagination() {
        let monitor = ProxyMonitor::new(100);
        for i in 0..10 {
            monitor.add_log(make_log(&format!("log-{}", i), 200));
        }

        let page1 = monitor.get_logs(0, 3);
        assert_eq!(page1.len(), 3);
        assert_eq!(page1[0].id, "log-9"); // newest first

        let page2 = monitor.get_logs(3, 3);
        assert_eq!(page2.len(), 3);
        assert_eq!(page2[0].id, "log-6");
    }

    #[test]
    fn monitor_clear() {
        let monitor = ProxyMonitor::new(100);
        monitor.add_log(make_log("log-1", 200));
        monitor.clear();
        assert_eq!(monitor.get_count(), 0);
    }
}
