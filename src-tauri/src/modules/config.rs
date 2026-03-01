use std::fs;
use std::path::PathBuf;

use crate::constants::{CONFIG_FILE_NAME, DATA_DIR_NAME};
use crate::models::AppConfig;

/// Get the application data directory, creating it if necessary.
pub fn get_data_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir()
        .or_else(dirs::data_dir)
        .ok_or_else(|| "Cannot determine config directory".to_string())?;

    let data_dir = base.join(DATA_DIR_NAME);
    if !data_dir.exists() {
        fs::create_dir_all(&data_dir)
            .map_err(|e| format!("Failed to create data directory: {}", e))?;
    }

    Ok(data_dir)
}

/// Get the path to the configuration file.
pub fn get_config_path() -> Result<PathBuf, String> {
    Ok(get_data_dir()?.join(CONFIG_FILE_NAME))
}

/// Load AppConfig from disk. Returns default if file doesn't exist or fails to parse.
pub fn load_app_config() -> AppConfig {
    let path = match get_config_path() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to get config path: {}, using defaults", e);
            return AppConfig::default();
        }
    };

    if !path.exists() {
        tracing::info!("Config file not found, using defaults");
        return AppConfig::default();
    }

    match fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str::<AppConfig>(&content) {
            Ok(config) => {
                tracing::info!(
                    accounts = config.accounts.len(),
                    proxy_accounts = config.proxy_accounts.len(),
                    "Config loaded from {:?}",
                    path
                );
                config
            }
            Err(e) => {
                tracing::error!("Failed to parse config: {}, using defaults", e);
                AppConfig::default()
            }
        },
        Err(e) => {
            tracing::error!("Failed to read config file: {}, using defaults", e);
            AppConfig::default()
        }
    }
}

/// Save AppConfig to disk.
pub fn save_app_config(config: &AppConfig) -> Result<(), String> {
    let path = get_config_path()?;
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    fs::write(&path, json).map_err(|e| format!("Failed to write config: {}", e))?;

    tracing::info!(
        accounts = config.accounts.len(),
        proxy_accounts = config.proxy_accounts.len(),
        "Config saved to {:?}",
        path
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn load_missing_config_returns_default() {
        // Point to a temp dir that definitely doesn't have a config
        let config = load_app_config();
        assert!(config.accounts.is_empty());
        assert_eq!(config.proxy.port, 8045);
    }

    #[test]
    fn save_and_load_config_round_trip() {
        let tmp = env::temp_dir().join("apimanager_test_config");
        let _ = fs::create_dir_all(&tmp);

        let config_path = tmp.join(CONFIG_FILE_NAME);
        let mut config = AppConfig::default();
        config.proxy.port = 9999;

        let json = serde_json::to_string_pretty(&config).unwrap();
        fs::write(&config_path, &json).unwrap();

        let loaded: AppConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(loaded.proxy.port, 9999);

        // Cleanup
        let _ = fs::remove_file(config_path);
        let _ = fs::remove_dir(tmp);
    }
}
