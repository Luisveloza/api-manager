//! Per-account circuit breaker with three states: Closed → Open → HalfOpen.
//!
//! When an account accumulates `failure_threshold` consecutive failures, the
//! circuit trips to Open. After a cooldown period (exponential backoff), it
//! transitions to HalfOpen and allows one probe request through. A successful
//! probe resets the circuit to Closed; a failed probe trips it back to Open
//! with an incremented trip_count (longer cooldown).

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before tripping Open.
    pub failure_threshold: u32,
    /// Base cooldown before transitioning from Open → HalfOpen.
    pub base_cooldown: Duration,
    /// Maximum cooldown (caps exponential backoff).
    pub max_cooldown: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            base_cooldown: Duration::from_secs(60),
            max_cooldown: Duration::from_secs(600),
        }
    }
}

// ---------------------------------------------------------------------------
// Circuit state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half_open",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "open" => Self::Open,
            "half_open" => Self::HalfOpen,
            _ => Self::Closed,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-account entry
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct CircuitEntry {
    state: CircuitState,
    consecutive_failures: u32,
    last_failure_time: Option<Instant>,
    trip_count: u32,
    failure_reason: String,
    dirty: bool,
}

impl CircuitEntry {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            last_failure_time: None,
            trip_count: 0,
            failure_reason: String::new(),
            dirty: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot for persistence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitSnapshot {
    pub state: String,
    pub consecutive_failures: u32,
    pub trip_count: u32,
    pub failure_reason: String,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

pub struct CircuitBreakerRegistry {
    entries: DashMap<String, CircuitEntry>,
    config: CircuitBreakerConfig,
    /// Callback invoked when a circuit trips to Open.
    /// Used to clear session affinities pointing at the tripped account.
    on_trip: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

impl CircuitBreakerRegistry {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            entries: DashMap::new(),
            config,
            on_trip: None,
        }
    }

    /// Register a callback that fires when any circuit trips to Open.
    pub fn set_on_trip<F>(&mut self, f: F)
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        self.on_trip = Some(Arc::new(f));
    }

    /// Returns `true` if the account is tripped (should NOT receive traffic).
    ///
    /// Side effect: if Open and cooldown has elapsed, transitions to HalfOpen
    /// and returns `false` (allowing one probe).
    pub fn is_tripped(&self, account_id: &str) -> bool {
        let mut entry = match self.entries.get_mut(account_id) {
            Some(e) => e,
            None => return false, // no entry = healthy
        };

        match entry.state {
            CircuitState::Closed => false,
            CircuitState::Open => {
                // Check cooldown
                if let Some(last_fail) = entry.last_failure_time {
                    let cooldown = self.cooldown_for(entry.trip_count);
                    if last_fail.elapsed() >= cooldown {
                        entry.state = CircuitState::HalfOpen;
                        entry.dirty = true;
                        tracing::info!(
                            account_id,
                            trip_count = entry.trip_count,
                            "Circuit breaker: Open → HalfOpen (cooldown elapsed)"
                        );
                        return false; // allow one probe
                    }
                }
                true // still in cooldown
            }
            CircuitState::HalfOpen => {
                // HalfOpen: only one probe allowed. Subsequent calls are tripped
                // until the probe resolves (success or failure).
                true
            }
        }
    }

    /// Record a successful request — fully reset the circuit.
    pub fn record_success(&self, account_id: &str) {
        let mut entry = self.entries.entry(account_id.to_string()).or_insert_with(CircuitEntry::new);
        let was_tripped = entry.state != CircuitState::Closed;
        entry.state = CircuitState::Closed;
        entry.consecutive_failures = 0;
        entry.trip_count = 0;
        entry.failure_reason.clear();
        entry.dirty = true;

        if was_tripped {
            tracing::info!(account_id, "Circuit breaker: → Closed (success)");
        }
    }

    /// Record a failure. Closed → accumulate; threshold → trip Open.
    /// HalfOpen → trip Open with incremented trip_count.
    pub fn record_failure(&self, account_id: &str, reason: &str) {
        let should_fire_on_trip;
        {
            let mut entry = self.entries.entry(account_id.to_string()).or_insert_with(CircuitEntry::new);
            entry.consecutive_failures += 1;
            entry.last_failure_time = Some(Instant::now());
            entry.failure_reason = reason.to_string();
            entry.dirty = true;

            match entry.state {
                CircuitState::Closed => {
                    if entry.consecutive_failures >= self.config.failure_threshold {
                        entry.state = CircuitState::Open;
                        entry.trip_count += 1;
                        should_fire_on_trip = true;
                        tracing::error!(
                            account_id,
                            consecutive_failures = entry.consecutive_failures,
                            trip_count = entry.trip_count,
                            reason = %entry.failure_reason,
                            "Circuit breaker: Closed → Open"
                        );
                    } else {
                        should_fire_on_trip = false;
                    }
                }
                CircuitState::HalfOpen => {
                    entry.trip_count += 1;
                    entry.state = CircuitState::Open;
                    should_fire_on_trip = true;
                    tracing::warn!(
                        account_id,
                        trip_count = entry.trip_count,
                        reason,
                        "Circuit breaker: HalfOpen → Open (probe failed)"
                    );
                }
                CircuitState::Open => {
                    should_fire_on_trip = false;
                }
            }
        } // entry lock released

        if should_fire_on_trip {
            self.fire_on_trip(account_id);
        }
    }

    /// Auth failure: trip immediately regardless of threshold.
    pub fn record_auth_failure(&self, account_id: &str) {
        let should_fire;
        {
            let mut entry = self.entries.entry(account_id.to_string()).or_insert_with(CircuitEntry::new);
            entry.consecutive_failures = self.config.failure_threshold;
            entry.last_failure_time = Some(Instant::now());
            entry.failure_reason = "auth_failed".to_string();
            entry.dirty = true;

            if entry.state != CircuitState::Open {
                entry.state = CircuitState::Open;
                entry.trip_count += 1;
                should_fire = true;
                tracing::error!(
                    account_id,
                    trip_count = entry.trip_count,
                    "Circuit breaker: → Open (auth failure)"
                );
            } else {
                should_fire = false;
            }
        }

        if should_fire {
            self.fire_on_trip(account_id);
        }
    }

    /// Drain all dirty entries and return snapshots for persistence.
    pub fn drain_dirty(&self) -> Vec<(String, CircuitSnapshot)> {
        let mut result = Vec::new();
        for mut entry in self.entries.iter_mut() {
            if entry.dirty {
                entry.dirty = false;
                result.push((
                    entry.key().clone(),
                    CircuitSnapshot {
                        state: entry.state.as_str().to_string(),
                        consecutive_failures: entry.consecutive_failures,
                        trip_count: entry.trip_count,
                        failure_reason: entry.failure_reason.clone(),
                    },
                ));
            }
        }
        result
    }

    /// Load persisted state for an account.
    pub fn load_persisted(
        &self,
        account_id: &str,
        circuit_state: Option<&str>,
        consecutive_failures: u32,
        trip_count: u32,
        failure_reason: &str,
        disabled_by_proxy: bool,
    ) {
        let state = match circuit_state {
            Some(s) => CircuitState::from_str_lossy(s),
            None => {
                // Migrate from old format
                if disabled_by_proxy {
                    CircuitState::Open
                } else if consecutive_failures > 0 {
                    CircuitState::Closed
                } else {
                    CircuitState::Closed
                }
            }
        };

        let trip = if circuit_state.is_none() && disabled_by_proxy {
            1.max(trip_count)
        } else {
            trip_count
        };

        let entry = CircuitEntry {
            state,
            consecutive_failures,
            last_failure_time: if state == CircuitState::Open {
                Some(Instant::now()) // start cooldown from now
            } else {
                None
            },
            trip_count: trip,
            failure_reason: failure_reason.to_string(),
            dirty: false,
        };

        self.entries.insert(account_id.to_string(), entry);
    }

    /// Remove an account from the registry.
    pub fn remove(&self, account_id: &str) {
        self.entries.remove(account_id);
    }

    /// Check if there are any dirty entries.
    pub fn has_dirty(&self) -> bool {
        self.entries.iter().any(|e| e.dirty)
    }

    /// Get the current state for an account (for display/API).
    pub fn get_state(&self, account_id: &str) -> Option<(CircuitState, u32, u32)> {
        self.entries.get(account_id).map(|e| {
            (e.state, e.consecutive_failures, e.trip_count)
        })
    }

    // -- Internal -----------------------------------------------------------

    fn fire_on_trip(&self, account_id: &str) {
        if let Some(ref cb) = self.on_trip {
            cb(account_id);
        }
    }

    fn cooldown_for(&self, trip_count: u32) -> Duration {
        if trip_count == 0 {
            return self.config.base_cooldown;
        }
        let exponent = (trip_count - 1).min(10);
        let multiplier = 2u64.saturating_pow(exponent);
        let base_ms = self.config.base_cooldown.as_millis() as u64;
        let cooldown_ms = base_ms.saturating_mul(multiplier);
        let max_ms = self.config.max_cooldown.as_millis() as u64;
        Duration::from_millis(cooldown_ms.min(max_ms))
    }
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_registry() -> CircuitBreakerRegistry {
        CircuitBreakerRegistry::new(CircuitBreakerConfig {
            failure_threshold: 5,
            base_cooldown: Duration::from_millis(100),
            max_cooldown: Duration::from_millis(1000),
        })
    }

    #[test]
    fn initial_state_is_closed() {
        let reg = make_registry();
        assert!(!reg.is_tripped("acc1"));
    }

    #[test]
    fn five_failures_trip_open() {
        let reg = make_registry();
        for _ in 0..4 {
            reg.record_failure("acc1", "upstream_error");
            assert!(!reg.is_tripped("acc1"));
        }
        reg.record_failure("acc1", "upstream_error");
        assert!(reg.is_tripped("acc1"));
    }

    #[test]
    fn cooldown_transitions_to_half_open() {
        let reg = CircuitBreakerRegistry::new(CircuitBreakerConfig {
            failure_threshold: 1,
            base_cooldown: Duration::from_millis(10),
            max_cooldown: Duration::from_millis(100),
        });
        reg.record_failure("acc1", "test");
        assert!(reg.is_tripped("acc1"));

        std::thread::sleep(Duration::from_millis(20));

        // After cooldown, should transition to HalfOpen and return false (probe allowed)
        assert!(!reg.is_tripped("acc1"));

        // Verify state is HalfOpen
        let (state, _, _) = reg.get_state("acc1").unwrap();
        assert_eq!(state, CircuitState::HalfOpen);
    }

    #[test]
    fn successful_probe_resets_to_closed() {
        let reg = CircuitBreakerRegistry::new(CircuitBreakerConfig {
            failure_threshold: 1,
            base_cooldown: Duration::from_millis(10),
            max_cooldown: Duration::from_millis(100),
        });
        reg.record_failure("acc1", "test");
        std::thread::sleep(Duration::from_millis(20));

        // Transition to HalfOpen
        assert!(!reg.is_tripped("acc1"));

        // Probe succeeds
        reg.record_success("acc1");
        let (state, failures, trips) = reg.get_state("acc1").unwrap();
        assert_eq!(state, CircuitState::Closed);
        assert_eq!(failures, 0);
        assert_eq!(trips, 0);
    }

    #[test]
    fn failed_probe_increments_trip_count() {
        let reg = CircuitBreakerRegistry::new(CircuitBreakerConfig {
            failure_threshold: 1,
            base_cooldown: Duration::from_millis(10),
            max_cooldown: Duration::from_millis(1000),
        });
        reg.record_failure("acc1", "test");
        std::thread::sleep(Duration::from_millis(20));

        // Transition to HalfOpen
        assert!(!reg.is_tripped("acc1"));

        // Probe fails
        reg.record_failure("acc1", "still_broken");

        let (state, _, trip_count) = reg.get_state("acc1").unwrap();
        assert_eq!(state, CircuitState::Open);
        assert_eq!(trip_count, 2); // initial trip + probe failure
    }

    #[test]
    fn exponential_backoff_cooldown() {
        let reg = make_registry();
        // base=100ms, max=1000ms
        assert_eq!(reg.cooldown_for(0), Duration::from_millis(100));
        assert_eq!(reg.cooldown_for(1), Duration::from_millis(100)); // 100 * 2^0
        assert_eq!(reg.cooldown_for(2), Duration::from_millis(200)); // 100 * 2^1
        assert_eq!(reg.cooldown_for(3), Duration::from_millis(400)); // 100 * 2^2
        assert_eq!(reg.cooldown_for(4), Duration::from_millis(800)); // 100 * 2^3
        assert_eq!(reg.cooldown_for(5), Duration::from_millis(1000)); // capped at max
        assert_eq!(reg.cooldown_for(10), Duration::from_millis(1000)); // still capped
    }

    #[test]
    fn auth_failure_trips_immediately() {
        let reg = make_registry();
        reg.record_auth_failure("acc1");
        assert!(reg.is_tripped("acc1"));

        let (state, failures, _) = reg.get_state("acc1").unwrap();
        assert_eq!(state, CircuitState::Open);
        assert_eq!(failures, 5); // set to threshold
    }

    #[test]
    fn success_resets_all() {
        let reg = make_registry();
        for _ in 0..3 {
            reg.record_failure("acc1", "test");
        }
        reg.record_success("acc1");
        let (state, failures, trips) = reg.get_state("acc1").unwrap();
        assert_eq!(state, CircuitState::Closed);
        assert_eq!(failures, 0);
        assert_eq!(trips, 0);
    }

    #[test]
    fn drain_dirty_returns_modified_entries() {
        let reg = make_registry();
        reg.record_failure("acc1", "test");
        reg.record_failure("acc2", "test");

        let dirty = reg.drain_dirty();
        assert_eq!(dirty.len(), 2);

        // Second drain should be empty
        let dirty2 = reg.drain_dirty();
        assert!(dirty2.is_empty());
    }

    #[test]
    fn load_persisted_migrates_old_format() {
        let reg = make_registry();
        // Old format: disabled_by_proxy=true, no circuit_state
        reg.load_persisted("acc1", None, 5, 0, "auth_failed", true);

        let (state, failures, trip_count) = reg.get_state("acc1").unwrap();
        assert_eq!(state, CircuitState::Open);
        assert_eq!(failures, 5);
        assert_eq!(trip_count, 1); // migrated
    }

    #[test]
    fn load_persisted_new_format() {
        let reg = make_registry();
        reg.load_persisted("acc1", Some("half_open"), 3, 2, "upstream_error", false);

        let (state, failures, trip_count) = reg.get_state("acc1").unwrap();
        assert_eq!(state, CircuitState::HalfOpen);
        assert_eq!(failures, 3);
        assert_eq!(trip_count, 2);
    }

    #[test]
    fn remove_clears_entry() {
        let reg = make_registry();
        reg.record_failure("acc1", "test");
        assert!(reg.get_state("acc1").is_some());

        reg.remove("acc1");
        assert!(reg.get_state("acc1").is_none());
        assert!(!reg.is_tripped("acc1"));
    }
}
