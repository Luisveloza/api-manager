use serde::{Deserialize, Serialize};

use crate::constants::{DEFAULT_PORT, DEFAULT_REQUEST_TIMEOUT};

// ============================================================================
// ProxyAuthMode
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProxyAuthMode {
    /// No auth required for any route
    Off,
    /// Auth required for all routes including /health
    Strict,
    /// Auth required for all routes except /health and /healthz
    AllExceptHealth,
    /// Automatic: Desktop → Off, Headless → AllExceptHealth
    Auto,
}

impl Default for ProxyAuthMode {
    fn default() -> Self {
        Self::Auto
    }
}

// ============================================================================
// UpstreamProxyConfig
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamProxyConfig {
    /// Whether upstream proxy is enabled
    #[serde(default)]
    pub enabled: bool,
    /// Proxy URL (http://, https://, socks5://)
    #[serde(default)]
    pub url: String,
}

impl Default for UpstreamProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
        }
    }
}

// ============================================================================
// LoadBalanceMode
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalanceMode {
    RoundRobin,
    Failover,
    Random,
    Weighted,
}

impl Default for LoadBalanceMode {
    fn default() -> Self {
        Self::RoundRobin
    }
}

// ============================================================================
// ProxyConfig
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Whether proxy service is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Listen port
    #[serde(default = "default_port")]
    pub port: u16,

    /// API key for proxy authentication
    #[serde(default = "generate_api_key")]
    pub api_key: String,

    /// Admin password for management routes (falls back to api_key if None)
    #[serde(default)]
    pub admin_password: Option<String>,

    /// Authentication mode
    #[serde(default)]
    pub auth_mode: ProxyAuthMode,

    /// Allow LAN access (bind to 0.0.0.0 instead of 127.0.0.1)
    #[serde(default)]
    pub allow_lan_access: bool,

    /// Auto-start proxy on application launch
    #[serde(default)]
    pub auto_start: bool,

    /// Upstream request timeout in seconds
    #[serde(default = "default_request_timeout")]
    pub request_timeout: u64,

    /// Enable request logging (monitoring)
    #[serde(default = "default_true")]
    pub enable_logging: bool,

    /// Upstream proxy configuration
    #[serde(default)]
    pub upstream_proxy: UpstreamProxyConfig,

    /// Load balancing mode
    #[serde(default)]
    pub load_balance_mode: LoadBalanceMode,

    /// Daily cost limit in USD. 0 = no limit.
    #[serde(default)]
    pub daily_cost_limit: f64,

    /// Monthly cost limit in USD. 0 = no limit.
    #[serde(default)]
    pub monthly_cost_limit: f64,

    /// Action when budget exceeded: "warn" or "block".
    #[serde(default = "default_budget_action")]
    pub budget_exceeded_action: String,

    /// Model alias mappings (pattern → target).
    #[serde(default)]
    pub model_aliases: Vec<ModelAlias>,

    /// Model-to-account routing rules.
    #[serde(default)]
    pub model_routes: Vec<ModelRoute>,

    /// Additional API keys for multi-user access.
    /// The main `api_key` always acts as admin and is not subject to these limits.
    #[serde(default)]
    pub api_keys: Vec<ProxyApiKey>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            api_key: generate_api_key(),
            admin_password: None,
            auth_mode: ProxyAuthMode::default(),
            allow_lan_access: false,
            auto_start: false,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            enable_logging: true,
            upstream_proxy: UpstreamProxyConfig::default(),
            load_balance_mode: LoadBalanceMode::default(),
            daily_cost_limit: 0.0,
            monthly_cost_limit: 0.0,
            budget_exceeded_action: "warn".to_string(),
            model_aliases: Vec::new(),
            model_routes: Vec::new(),
            api_keys: Vec::new(),
        }
    }
}

impl ProxyConfig {
    /// Get the bind address based on allow_lan_access
    pub fn get_bind_address(&self) -> &str {
        if self.allow_lan_access {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        }
    }
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_request_timeout() -> u64 {
    DEFAULT_REQUEST_TIMEOUT
}

fn default_budget_action() -> String {
    "warn".to_string()
}

/// Model alias: maps a request model name to the real upstream model name.
/// Supports simple glob: `*` matches any substring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAlias {
    pub pattern: String,
    pub target: String,
}

/// Model route: forces specific models to go to specific accounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoute {
    pub model_pattern: String,
    pub account_ids: Vec<String>,
    #[serde(default)]
    pub priority: i32,
}

/// A proxy API key with per-key settings for multi-user access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyApiKey {
    /// The API key string (e.g., "sk-user1-xxxx").
    pub key: String,
    /// Human-readable label (e.g., "Alice's key").
    pub label: String,
    /// Whether this key is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Daily cost limit in USD. 0 = unlimited.
    #[serde(default)]
    pub daily_limit: f64,
    /// Monthly cost limit in USD. 0 = unlimited.
    #[serde(default)]
    pub monthly_limit: f64,
    /// Allowed models (empty = all).
    #[serde(default)]
    pub allowed_models: Vec<String>,
    /// Created timestamp (unix seconds).
    #[serde(default)]
    pub created_at: i64,
}

fn default_true() -> bool {
    true
}

fn generate_api_key() -> String {
    format!("sk-{}", uuid::Uuid::new_v4().simple())
}

// ============================================================================
// Account Models (unchanged from architecture doc)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub id: i64,
    pub access_token: String,
    /// API Key fetched from upstream (e.g. `sk-xxx`). Used for AI API calls.
    /// For sub2api, this is None — the JWT access_token doubles as the API key.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub quota: f64,
    #[serde(default)]
    pub today_prompt_tokens: u64,
    #[serde(default)]
    pub today_completion_tokens: u64,
    #[serde(default)]
    pub today_quota_consumption: f64,
    #[serde(default)]
    pub today_requests_count: u64,
    #[serde(default)]
    pub today_income: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Proxy runtime health state — persisted to disk so that failed accounts
/// stay disabled across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyHealthState {
    /// Current health score (0.0 = dead, 1.0 = healthy).
    #[serde(default = "default_health_score")]
    pub health_score: f32,
    /// Unix timestamp (seconds) of the last failure. 0 means never failed.
    #[serde(default)]
    pub last_failure_time: i64,
    /// Human-readable reason for the last failure (e.g. "connection_timeout", "auth_401").
    #[serde(default)]
    pub failure_reason: String,
    /// Number of consecutive failures without a success in between.
    #[serde(default)]
    pub consecutive_failures: u32,
    /// Whether the proxy has automatically disabled this account due to persistent failures.
    /// Kept for backward compatibility — driven by circuit_state=="open".
    #[serde(default)]
    pub disabled_by_proxy: bool,
    /// Circuit breaker state: "closed", "open", or "half_open".
    /// When absent (old format), inferred from disabled_by_proxy + consecutive_failures.
    #[serde(default)]
    pub circuit_state: Option<String>,
    /// Number of times the circuit has tripped to Open (for exponential backoff).
    #[serde(default)]
    pub trip_count: u32,
}

fn default_health_score() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteAccount {
    pub id: String,
    pub site_name: String,
    pub site_url: String,
    pub site_type: String,
    pub account_info: AccountInfo,

    /// Authentication type: "access_token" | "cookie" | "none"
    #[serde(alias = "authType", default = "default_auth_type")]
    pub auth_type: String,

    #[serde(default)]
    pub last_sync_time: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub health: Option<HealthStatus>,
    #[serde(default)]
    pub exchange_rate: Option<f64>,
    /// Proxy runtime health state — persisted so failed accounts stay disabled across restarts.
    #[serde(default)]
    pub proxy_health: Option<ProxyHealthState>,
    /// Priority for failover load balancing (lower = higher priority). Default 0.
    #[serde(default)]
    pub proxy_priority: i32,
    /// Weight for weighted load balancing (1-100). Default 10.
    #[serde(default = "default_proxy_weight")]
    pub proxy_weight: u32,
}

fn default_proxy_weight() -> u32 {
    10
}

fn default_auth_type() -> String {
    "access_token".to_string()
}

// ============================================================================
// AppConfig (top-level config)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Proxy service configuration
    #[serde(default)]
    pub proxy: ProxyConfig,

    /// All imported accounts
    #[serde(default)]
    pub accounts: Vec<SiteAccount>,

    /// Accounts enabled for proxy (subset of accounts)
    #[serde(default)]
    pub proxy_accounts: Vec<SiteAccount>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            proxy: ProxyConfig::default(),
            accounts: Vec::new(),
            proxy_accounts: Vec::new(),
        }
    }
}

// ============================================================================
// Backup V2 format (for deserialization of All API Hub exports)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupV2 {
    pub version: String,
    pub timestamp: i64,
    #[serde(rename = "type")]
    pub backup_type: String,
    pub accounts: BackupV2Accounts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupV2Accounts {
    pub accounts: Vec<serde_json::Value>,
    #[serde(default)]
    pub bookmarks: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub last_updated: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_config_defaults() {
        let config = ProxyConfig::default();
        assert_eq!(config.port, 8045);
        assert!(!config.enabled);
        assert!(!config.allow_lan_access);
        assert_eq!(config.request_timeout, 120);
        assert!(config.enable_logging);
        assert!(config.api_key.starts_with("sk-"));
    }

    #[test]
    fn proxy_config_bind_address() {
        let mut config = ProxyConfig::default();
        assert_eq!(config.get_bind_address(), "127.0.0.1");
        config.allow_lan_access = true;
        assert_eq!(config.get_bind_address(), "0.0.0.0");
    }

    #[test]
    fn proxy_auth_mode_serde() {
        let json = r#""all_except_health""#;
        let mode: ProxyAuthMode = serde_json::from_str(json).unwrap();
        assert_eq!(mode, ProxyAuthMode::AllExceptHealth);

        let serialized = serde_json::to_string(&ProxyAuthMode::Strict).unwrap();
        assert_eq!(serialized, r#""strict""#);
    }

    #[test]
    fn app_config_round_trip() {
        let config = AppConfig::default();
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.proxy.port, config.proxy.port);
        assert_eq!(parsed.accounts.len(), 0);
    }

    #[test]
    fn proxy_health_state_serde_defaults() {
        // Missing proxy_health field → None
        let json = r#"{"id":"x","site_name":"s","site_url":"u","site_type":"new-api","account_info":{"id":1,"access_token":"sk-x"}}"#;
        let account: SiteAccount = serde_json::from_str(json).unwrap();
        assert!(account.proxy_health.is_none());

        // Present but partial → defaults fill in
        let json2 = r#"{"id":"x","site_name":"s","site_url":"u","site_type":"new-api","account_info":{"id":1,"access_token":"sk-x"},"proxy_health":{"health_score":0.0,"disabled_by_proxy":true}}"#;
        let account2: SiteAccount = serde_json::from_str(json2).unwrap();
        let ph = account2.proxy_health.unwrap();
        assert_eq!(ph.health_score, 0.0);
        assert!(ph.disabled_by_proxy);
        assert_eq!(ph.consecutive_failures, 0); // default
    }

    #[test]
    fn app_config_deserialize_with_missing_proxy() {
        // Old-style config without nested proxy — should use defaults
        let json = r#"{"accounts": [], "proxy_accounts": []}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.proxy.port, 8045);
        assert!(!config.proxy.enabled);
    }
}
