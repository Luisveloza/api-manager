use std::time::Duration;

/// Retry strategy for failed upstream requests.
#[derive(Debug, Clone)]
pub enum RetryStrategy {
    NoRetry,
    FixedDelay(u64),           // delay_ms
    LinearBackoff(u64, u64),   // base_ms, max_ms
    ExponentialBackoff(u64, u64), // base_ms, max_ms
}

/// Determine retry strategy based on HTTP status and error text.
pub fn determine_retry_strategy(status: u16, _error_text: &str) -> RetryStrategy {
    match status {
        401 | 403 => RetryStrategy::FixedDelay(100), // auth error — rotate fast
        404 => RetryStrategy::FixedDelay(100),        // model not found — rotate fast
        429 | 529 => RetryStrategy::ExponentialBackoff(1000, 10000),
        500 | 502 | 503 => RetryStrategy::FixedDelay(2000),
        504 => RetryStrategy::FixedDelay(3000),
        _ => RetryStrategy::NoRetry,
    }
}

/// Returns the rate-limit cooldown duration for a given error status.
/// Auth errors (401/403) are considered permanent and get a long cooldown (10 min).
/// 404 (model not found) gets no cooldown — the account is fine, just not for this model.
/// Transient errors (429/5xx) keep the short default (60s or Retry-After header).
pub fn rate_limit_duration_for_status(status: u16, retry_after: Option<Duration>) -> Duration {
    match status {
        401 | 403 => Duration::from_secs(600), // 10 minutes — auth is broken, stop hammering
        404 => Duration::from_secs(0),         // no cooldown — account works, model doesn't
        _ => retry_after.unwrap_or(Duration::from_secs(60)),
    }
}

/// Returns true if the error is a permanent auth failure (token invalid/revoked).
pub fn is_auth_error(status: u16) -> bool {
    matches!(status, 401 | 403)
}

/// Apply the retry strategy: sleep for the appropriate delay.
/// Returns true if the retry should proceed, false if max delay exceeded.
pub async fn apply_retry_strategy(strategy: &RetryStrategy, attempt: usize) -> bool {
    let delay = match strategy {
        RetryStrategy::NoRetry => return false,
        RetryStrategy::FixedDelay(ms) => *ms,
        RetryStrategy::LinearBackoff(base, max) => {
            let d = base.saturating_mul(attempt as u64 + 1);
            d.min(*max)
        }
        RetryStrategy::ExponentialBackoff(base, max) => {
            let d = base.saturating_mul(2u64.saturating_pow(attempt as u32));
            d.min(*max)
        }
    };

    tokio::time::sleep(Duration::from_millis(delay)).await;
    true
}

/// Determine if the error status warrants rotating to a different account.
/// 404 is included because upstream "model not found" should try another account
/// that may actually support the requested model.
pub fn should_rotate_account(status: u16) -> bool {
    matches!(status, 401 | 403 | 404 | 429 | 500 | 502 | 503 | 529)
}

/// Baseline maximum retry attempts (fallback when active count is unknown).
pub const BASE_MAX_RETRIES: usize = 3;

/// Hard upper limit — never retry more than this many times regardless of pool size.
const MAX_RETRY_CAP: usize = 10;

/// Compute the effective max retries based on the number of active healthy accounts.
/// Ensures we can cycle through all healthy accounts at least once, capped at MAX_RETRY_CAP.
pub fn effective_max_retries(active_count: usize) -> usize {
    active_count.max(BASE_MAX_RETRIES).min(MAX_RETRY_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_retry_on_429() {
        let strategy = determine_retry_strategy(429, "rate limited");
        assert!(matches!(strategy, RetryStrategy::ExponentialBackoff(_, _)));
    }

    #[test]
    fn should_retry_on_401() {
        let strategy = determine_retry_strategy(401, "invalid token");
        assert!(matches!(strategy, RetryStrategy::FixedDelay(100)));
    }

    #[test]
    fn should_not_retry_on_400() {
        let strategy = determine_retry_strategy(400, "bad request");
        assert!(matches!(strategy, RetryStrategy::NoRetry));
    }

    #[test]
    fn should_retry_on_404_model_not_found() {
        let strategy = determine_retry_strategy(404, "model not found");
        assert!(matches!(strategy, RetryStrategy::FixedDelay(100)));
    }

    #[test]
    fn should_rotate_on_server_error() {
        assert!(should_rotate_account(401));
        assert!(should_rotate_account(403));
        assert!(should_rotate_account(404));
        assert!(should_rotate_account(429));
        assert!(should_rotate_account(500));
        assert!(should_rotate_account(502));
        assert!(should_rotate_account(503));
        assert!(!should_rotate_account(400));
        assert!(!should_rotate_account(200));
    }

    #[test]
    fn auth_error_gets_long_cooldown() {
        let d = rate_limit_duration_for_status(401, None);
        assert_eq!(d, Duration::from_secs(600));
        let d = rate_limit_duration_for_status(403, None);
        assert_eq!(d, Duration::from_secs(600));
        // Auth errors ignore retry-after header
        let d = rate_limit_duration_for_status(401, Some(Duration::from_secs(5)));
        assert_eq!(d, Duration::from_secs(600));
    }

    #[test]
    fn model_not_found_gets_zero_cooldown() {
        let d = rate_limit_duration_for_status(404, None);
        assert_eq!(d, Duration::from_secs(0));
        let d = rate_limit_duration_for_status(404, Some(Duration::from_secs(60)));
        assert_eq!(d, Duration::from_secs(0));
    }

    #[test]
    fn transient_error_uses_retry_after_or_default() {
        let d = rate_limit_duration_for_status(429, Some(Duration::from_secs(120)));
        assert_eq!(d, Duration::from_secs(120));
        let d = rate_limit_duration_for_status(500, None);
        assert_eq!(d, Duration::from_secs(60));
    }

    #[test]
    fn is_auth_error_classification() {
        assert!(is_auth_error(401));
        assert!(is_auth_error(403));
        assert!(!is_auth_error(429));
        assert!(!is_auth_error(500));
        assert!(!is_auth_error(200));
    }

    #[test]
    fn effective_max_retries_uses_baseline_for_small_pools() {
        assert_eq!(effective_max_retries(0), BASE_MAX_RETRIES);
        assert_eq!(effective_max_retries(1), BASE_MAX_RETRIES);
        assert_eq!(effective_max_retries(2), BASE_MAX_RETRIES);
        assert_eq!(effective_max_retries(3), 3);
    }

    #[test]
    fn effective_max_retries_scales_with_pool() {
        assert_eq!(effective_max_retries(5), 5);
        assert_eq!(effective_max_retries(8), 8);
        assert_eq!(effective_max_retries(10), 10);
    }

    #[test]
    fn effective_max_retries_caps_at_limit() {
        assert_eq!(effective_max_retries(11), 10);
        assert_eq!(effective_max_retries(50), 10);
        assert_eq!(effective_max_retries(100), 10);
    }
}
