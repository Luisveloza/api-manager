use dashmap::DashMap;
use std::time::{Duration, Instant};

/// Record of a rate-limited account
#[derive(Debug, Clone)]
pub struct RateLimitRecord {
    pub account_id: String,
    pub limited_at: Instant,
    pub retry_after: Duration,
    pub status_code: u16,
}

impl RateLimitRecord {
    pub fn is_expired(&self) -> bool {
        self.limited_at.elapsed() >= self.retry_after
    }

    pub fn remaining_secs(&self) -> u64 {
        self.retry_after
            .checked_sub(self.limited_at.elapsed())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// Tracks rate-limited accounts to skip them during token selection.
pub struct RateLimitTracker {
    records: DashMap<String, RateLimitRecord>,
}

impl RateLimitTracker {
    pub fn new() -> Self {
        Self {
            records: DashMap::new(),
        }
    }

    /// Mark an account as rate-limited.
    pub fn mark_limited(
        &self,
        account_id: &str,
        status: u16,
        retry_after: Option<Duration>,
    ) {
        let duration = retry_after.unwrap_or(Duration::from_secs(60));
        let record = RateLimitRecord {
            account_id: account_id.to_string(),
            limited_at: Instant::now(),
            retry_after: duration,
            status_code: status,
        };
        tracing::warn!(
            account_id,
            status,
            retry_secs = duration.as_secs(),
            "Account rate-limited"
        );
        self.records.insert(account_id.to_string(), record);
    }

    /// Check if an account is currently rate-limited.
    pub fn is_limited(&self, account_id: &str) -> bool {
        self.records
            .get(account_id)
            .map(|r| !r.is_expired())
            .unwrap_or(false)
    }

    /// Clear rate limit for a specific account.
    pub fn clear(&self, account_id: &str) {
        self.records.remove(account_id);
    }

    /// Clear all rate limits.
    pub fn clear_all(&self) {
        self.records.clear();
    }

    /// Remove expired records. Returns the number of records cleaned up.
    pub fn cleanup_expired(&self) -> usize {
        let before = self.records.len();
        self.records.retain(|_, v| !v.is_expired());
        let removed = before - self.records.len();
        if removed > 0 {
            tracing::debug!(removed, "Cleaned up expired rate limit records");
        }
        removed
    }

    /// Get remaining wait time in seconds (0 if not limited).
    pub fn remaining_wait(&self, account_id: &str) -> u64 {
        self.records
            .get(account_id)
            .map(|r| r.remaining_secs())
            .unwrap_or(0)
    }

    /// Current number of tracked records.
    pub fn len(&self) -> usize {
        self.records.len()
    }
}

impl Default for RateLimitTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_mark_and_check() {
        let tracker = RateLimitTracker::new();
        assert!(!tracker.is_limited("acc1"));

        tracker.mark_limited("acc1", 429, Some(Duration::from_secs(60)));
        assert!(tracker.is_limited("acc1"));
        assert!(!tracker.is_limited("acc2"));
    }

    #[test]
    fn rate_limit_expires() {
        let tracker = RateLimitTracker::new();
        // Use a very short duration so it expires immediately
        tracker.mark_limited("acc1", 429, Some(Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));
        assert!(!tracker.is_limited("acc1"));
    }

    #[test]
    fn rate_limit_cleanup() {
        let tracker = RateLimitTracker::new();
        tracker.mark_limited("acc1", 429, Some(Duration::from_millis(1)));
        tracker.mark_limited("acc2", 429, Some(Duration::from_secs(300)));
        std::thread::sleep(Duration::from_millis(5));

        let cleaned = tracker.cleanup_expired();
        assert_eq!(cleaned, 1);
        assert_eq!(tracker.len(), 1);
        assert!(!tracker.is_limited("acc1"));
        assert!(tracker.is_limited("acc2"));
    }

    #[test]
    fn rate_limit_clear() {
        let tracker = RateLimitTracker::new();
        tracker.mark_limited("acc1", 429, Some(Duration::from_secs(60)));
        tracker.clear("acc1");
        assert!(!tracker.is_limited("acc1"));
    }

    #[test]
    fn rate_limit_clear_all() {
        let tracker = RateLimitTracker::new();
        tracker.mark_limited("acc1", 429, Some(Duration::from_secs(60)));
        tracker.mark_limited("acc2", 503, Some(Duration::from_secs(30)));
        tracker.clear_all();
        assert_eq!(tracker.len(), 0);
    }
}
