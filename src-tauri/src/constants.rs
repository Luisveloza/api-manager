/// Default proxy listen port
pub const DEFAULT_PORT: u16 = 8045;

/// Default upstream request timeout in seconds
pub const DEFAULT_REQUEST_TIMEOUT: u64 = 120;

/// Application data directory name
pub const DATA_DIR_NAME: &str = "APIManager";

/// Configuration file name
pub const CONFIG_FILE_NAME: &str = "apimanager_config.json";

/// Application version from Cargo.toml
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
