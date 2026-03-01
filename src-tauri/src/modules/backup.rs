use std::path::Path;

use crate::models::{BackupV2, SiteAccount};

/// Import accounts from a backup file path.
pub fn import_backup_from_path(path: &Path) -> Result<Vec<SiteAccount>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;
    import_backup_from_str(&content)
}

/// Import accounts from a JSON string (V2 format).
pub fn import_backup_from_str(json: &str) -> Result<Vec<SiteAccount>, String> {
    let backup: BackupV2 =
        serde_json::from_str(json).map_err(|e| format!("Failed to parse backup JSON: {}", e))?;

    if backup.version != "2.0" {
        return Err(format!(
            "Unsupported backup version: {} (expected 2.0)",
            backup.version
        ));
    }

    normalize_accounts(&backup.accounts.accounts)
}

/// Normalize raw account JSON values into strongly-typed SiteAccount structs.
pub fn normalize_accounts(raw_accounts: &[serde_json::Value]) -> Result<Vec<SiteAccount>, String> {
    let mut accounts = Vec::new();

    for (i, raw) in raw_accounts.iter().enumerate() {
        match normalize_single_account(raw) {
            Ok(account) => accounts.push(account),
            Err(e) => {
                tracing::warn!(index = i, error = %e, "Skipping invalid account entry");
            }
        }
    }

    tracing::info!(
        total = raw_accounts.len(),
        parsed = accounts.len(),
        "Accounts normalized"
    );

    Ok(accounts)
}

fn normalize_single_account(raw: &serde_json::Value) -> Result<SiteAccount, String> {
    // Required fields
    let site_url = raw["site_url"]
        .as_str()
        .ok_or("missing site_url")?
        .to_string();
    let site_name = raw["site_name"]
        .as_str()
        .unwrap_or("Unknown")
        .to_string();
    let site_type = raw["site_type"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();

    // Account info is required
    let account_info_val = raw
        .get("account_info")
        .ok_or("missing account_info")?;
    let account_info: crate::models::AccountInfo =
        serde_json::from_value(account_info_val.clone())
            .map_err(|e| format!("invalid account_info: {}", e))?;

    // Generate ID if missing
    let id = raw["id"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Auth type
    let auth_type = raw["authType"]
        .as_str()
        .or_else(|| raw["auth_type"].as_str())
        .unwrap_or("access_token")
        .to_string();

    // Health status
    let health = raw
        .get("health")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let disabled = raw["disabled"].as_bool();
    let exchange_rate = raw["exchange_rate"].as_f64();
    let notes = raw["notes"].as_str().map(|s| s.to_string());
    let last_sync_time = raw["last_sync_time"].as_i64().unwrap_or(0);
    let updated_at = raw["updated_at"].as_i64().unwrap_or(0);
    let created_at = raw["created_at"].as_i64().unwrap_or(0);

    Ok(SiteAccount {
        id,
        site_name,
        site_url,
        site_type,
        account_info,
        auth_type,
        last_sync_time,
        updated_at,
        created_at,
        notes,
        disabled,
        health,
        exchange_rate,
        proxy_health: None,
        proxy_priority: 0,
        proxy_weight: 10,
    })
}

/// Filter accounts suitable for proxying:
/// - Not disabled
/// - Has an access_token
/// - Health is not "error"
pub fn filter_proxy_accounts(accounts: &[SiteAccount]) -> Vec<SiteAccount> {
    accounts
        .iter()
        .filter(|a| {
            // Not disabled
            !a.disabled.unwrap_or(false)
            // Has access token
            && !a.account_info.access_token.is_empty()
            // Health is not explicitly errored
            && a.health
                .as_ref()
                .map(|h| h.status != "error")
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set env `BACKUP_FIXTURE_PATH` to a real backup JSON file to run this test.
    /// Example: BACKUP_FIXTURE_PATH=/path/to/accounts-backup.json cargo test
    #[test]
    fn import_from_fixture_file() {
        let path = match std::env::var("BACKUP_FIXTURE_PATH") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => return, // Skip when env var not set (CI, other devs)
        };
        assert!(path.exists(), "BACKUP_FIXTURE_PATH points to non-existent file: {path:?}");
        let accounts = import_backup_from_path(&path).unwrap();
        assert!(!accounts.is_empty(), "Should parse at least one account");
        let first = &accounts[0];
        assert!(!first.site_url.is_empty());
        assert!(!first.account_info.access_token.is_empty());
    }

    #[test]
    fn import_minimal_v2_json() {
        let json = r#"{
            "version": "2.0",
            "timestamp": 1234567890,
            "type": "accounts",
            "accounts": {
                "accounts": [
                    {
                        "site_name": "Test API",
                        "site_url": "https://api.test.com",
                        "site_type": "new-api",
                        "authType": "access_token",
                        "account_info": {
                            "id": 1,
                            "access_token": "sk-test",
                            "username": "test_user",
                            "quota": 1000.0,
                            "today_prompt_tokens": 0,
                            "today_completion_tokens": 0,
                            "today_quota_consumption": 0.0,
                            "today_requests_count": 0,
                            "today_income": 0.0
                        }
                    }
                ]
            }
        }"#;

        let accounts = import_backup_from_str(json).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].site_name, "Test API");
        assert_eq!(accounts[0].site_url, "https://api.test.com");
        assert_eq!(accounts[0].site_type, "new-api");
        assert_eq!(accounts[0].account_info.access_token, "sk-test");
    }

    #[test]
    fn reject_v1_backup() {
        let json = r#"{"version": "1.0", "timestamp": 0, "type": "accounts", "accounts": {"accounts": []}}"#;
        let result = import_backup_from_str(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported backup version"));
    }

    #[test]
    fn filter_proxy_accounts_skips_disabled() {
        let accounts = vec![
            SiteAccount {
                id: "1".into(),
                site_name: "A".into(),
                site_url: "https://a.com".into(),
                site_type: "new-api".into(),
                account_info: crate::models::AccountInfo {
                    id: 1,
                    access_token: "sk-a".into(),
                    api_key: None,
                    username: "a".into(),
                    quota: 100.0,
                    today_prompt_tokens: 0,
                    today_completion_tokens: 0,
                    today_quota_consumption: 0.0,
                    today_requests_count: 0,
                    today_income: 0.0,
                },
                auth_type: "access_token".into(),
                last_sync_time: 0,
                updated_at: 0,
                created_at: 0,
                notes: None,
                disabled: Some(true),
                health: None,
                exchange_rate: None,
                proxy_health: None,
                proxy_priority: 0,
                proxy_weight: 10,
            },
            SiteAccount {
                id: "2".into(),
                site_name: "B".into(),
                site_url: "https://b.com".into(),
                site_type: "new-api".into(),
                account_info: crate::models::AccountInfo {
                    id: 2,
                    access_token: "sk-b".into(),
                    api_key: None,
                    username: "b".into(),
                    quota: 200.0,
                    today_prompt_tokens: 0,
                    today_completion_tokens: 0,
                    today_quota_consumption: 0.0,
                    today_requests_count: 0,
                    today_income: 0.0,
                },
                auth_type: "access_token".into(),
                last_sync_time: 0,
                updated_at: 0,
                created_at: 0,
                notes: None,
                disabled: None,
                health: None,
                exchange_rate: None,
                proxy_health: None,
                proxy_priority: 0,
                proxy_weight: 10,
            },
        ];

        let proxy = filter_proxy_accounts(&accounts);
        assert_eq!(proxy.len(), 1);
        assert_eq!(proxy[0].id, "2");
    }
}
