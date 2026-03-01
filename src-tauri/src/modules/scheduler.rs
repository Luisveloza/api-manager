use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::proxy::rate_limit::RateLimitTracker;

/// Background scheduler for periodic maintenance tasks.
pub struct Scheduler {
    cancel_token: CancellationToken,
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            cancel_token: CancellationToken::new(),
            handles: Vec::new(),
        }
    }

    /// Start the rate-limit cleanup task (every 15s).
    pub fn start_rate_limit_cleanup(&mut self, tracker: Arc<RateLimitTracker>) {
        let cancel = self.cancel_token.child_token();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {
                        tracker.cleanup_expired();
                    }
                }
            }
        });
        self.handles.push(handle);
    }

    /// Start the log cleanup task (daily).
    pub fn start_log_cleanup(&mut self, max_age_days: u64) {
        let cancel = self.cancel_token.child_token();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(86400)); // daily
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {
                        if let Err(e) = crate::modules::logger::cleanup_old_logs(max_age_days) {
                            tracing::warn!("Log cleanup error: {}", e);
                        }
                    }
                }
            }
        });
        self.handles.push(handle);
    }

    /// Graceful shutdown: cancel all tasks and wait.
    pub async fn shutdown(&self) {
        self.cancel_token.cancel();
        // Wait briefly for tasks to finish
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
