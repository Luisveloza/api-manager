//! Fetch API Keys from upstream sites using access_token (management credential).
//!
//! The access_token is a management-plane credential used to query account info,
//! list tokens, etc. The API Key (e.g. `sk-xxx`) is the data-plane credential
//! used to call AI APIs (`/v1/chat/completions`, `/v1/messages`, etc.).
//!
//! This module bridges the two: given an access_token, it calls the upstream
//! token listing endpoint and extracts a usable API Key.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;

/// A single token entry from the upstream `/api/token/` response.
#[derive(Debug, Deserialize)]
struct TokenEntry {
    #[serde(default)]
    key: String,
    /// 1 = enabled, other values = disabled/expired.
    #[serde(default = "default_status")]
    status: i64,
}

fn default_status() -> i64 {
    1
}

/// Build the fan-out user-id headers that various New-API forks expect.
fn build_user_id_headers(user_id: i64) -> HeaderMap {
    let id_str = user_id.to_string();
    let mut headers = HeaderMap::new();

    let names = [
        "New-API-User",
        "Veloera-User",
        "voapi-user",
        "User-id",
        "Rix-Api-User",
        "neo-api-user",
    ];

    for name in names {
        if let (Ok(header_name), Ok(header_value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&id_str),
        ) {
            headers.insert(header_name, header_value);
        }
    }

    headers
}

/// Extract token entries from the upstream JSON response.
///
/// Handles multiple response shapes:
/// - Direct array: `[{ "key": "sk-xxx", "status": 1 }, ...]`
/// - New-API paginated: `{ "data": { "items": [...] } }`
/// - New-API fork paginated: `{ "data": { "data": [...] } }`
/// - OneHub paginated: `{ "data": [...] }`
fn parse_token_entries(body: &serde_json::Value) -> Vec<TokenEntry> {
    // Try: top-level is an array
    if let Some(arr) = body.as_array() {
        return arr
            .iter()
            .filter_map(|v| serde_json::from_value::<TokenEntry>(v.clone()).ok())
            .collect();
    }

    // Try: { "data": ... }
    if let Some(data) = body.get("data") {
        // data is array directly
        if let Some(arr) = data.as_array() {
            return arr
                .iter()
                .filter_map(|v| serde_json::from_value::<TokenEntry>(v.clone()).ok())
                .collect();
        }
        // data is object — try "items" then "data" as the array key
        for key in &["items", "data"] {
            if let Some(arr) = data.get(*key).and_then(|i| i.as_array()) {
                return arr
                    .iter()
                    .filter_map(|v| serde_json::from_value::<TokenEntry>(v.clone()).ok())
                    .collect();
            }
        }
    }

    Vec::new()
}

/// Select the first usable API key from a list of token entries.
/// A token is usable if `status == 1` and `key` is non-empty.
fn select_first_usable_key(entries: &[TokenEntry]) -> Option<String> {
    entries
        .iter()
        .find(|t| t.status == 1 && !t.key.is_empty())
        .map(|t| t.key.clone())
}

/// Fetch a usable API Key for the given account from its upstream site.
///
/// - For `sub2api`: returns the access_token as-is (the JWT is the API key).
/// - For all other site types: calls `GET /api/token/` with the access_token
///   and user-id headers, then picks the first enabled key.
pub async fn fetch_api_key(
    client: &reqwest::Client,
    site_url: &str,
    site_type: &str,
    access_token: &str,
    user_id: i64,
) -> Result<String, String> {
    // Sub2API: JWT doubles as the API key — no separate fetch needed.
    if site_type == "sub2api" {
        return Ok(access_token.to_string());
    }

    let url = format!(
        "{}/api/token/?p=0&size=100",
        site_url.trim_end_matches('/')
    );

    let mut headers = build_user_id_headers(user_id);
    if let Ok(auth_value) = HeaderValue::from_str(access_token) {
        headers.insert(reqwest::header::AUTHORIZATION, auth_value);
    }
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );

    let resp = client
        .get(&url)
        .headers(headers)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch tokens from {}: {}", site_url, e))?;

    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(format!(
            "Auth failed fetching tokens from {} (HTTP {})",
            site_url, status
        ));
    }
    if !(200..300).contains(&status) {
        return Err(format!(
            "Unexpected status {} fetching tokens from {}",
            status, site_url
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response from {}: {}", site_url, e))?;

    // Some upstreams return HTTP 200 with {"success": false, "message": "..."}
    // when the access_token is invalid/expired. Surface their message directly.
    if body.get("success").and_then(|v| v.as_bool()) == Some(false) {
        let msg = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(format!("{} responded: {}", site_url, msg));
    }

    let entries = parse_token_entries(&body);
    if entries.is_empty() {
        tracing::debug!(site_url, body = %body, "Token response parsed to empty list");
        return Err(format!("No tokens returned from {}", site_url));
    }

    select_first_usable_key(&entries)
        .ok_or_else(|| format!("No enabled tokens found on {}", site_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direct_array() {
        let json = serde_json::json!([
            { "key": "sk-aaa", "status": 1 },
            { "key": "sk-bbb", "status": 2 },
            { "key": "sk-ccc", "status": 1 },
        ]);
        let entries = parse_token_entries(&json);
        assert_eq!(entries.len(), 3);
        assert_eq!(select_first_usable_key(&entries), Some("sk-aaa".to_string()));
    }

    #[test]
    fn parse_newapi_paginated() {
        let json = serde_json::json!({
            "success": true,
            "data": {
                "items": [
                    { "key": "sk-disabled", "status": 2 },
                    { "key": "sk-good", "status": 1 },
                ]
            }
        });
        let entries = parse_token_entries(&json);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            select_first_usable_key(&entries),
            Some("sk-good".to_string())
        );
    }

    #[test]
    fn parse_onehub_data_array() {
        let json = serde_json::json!({
            "data": [
                { "key": "sk-hub1", "status": 1 },
                { "key": "sk-hub2", "status": 1 },
            ]
        });
        let entries = parse_token_entries(&json);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            select_first_usable_key(&entries),
            Some("sk-hub1".to_string())
        );
    }

    #[test]
    fn parse_empty_response() {
        let json = serde_json::json!({ "data": { "items": [] } });
        let entries = parse_token_entries(&json);
        assert!(entries.is_empty());
        assert_eq!(select_first_usable_key(&entries), None);
    }

    #[test]
    fn parse_all_disabled() {
        let json = serde_json::json!([
            { "key": "sk-off1", "status": 0 },
            { "key": "sk-off2", "status": 3 },
        ]);
        let entries = parse_token_entries(&json);
        assert_eq!(entries.len(), 2);
        assert_eq!(select_first_usable_key(&entries), None);
    }

    #[test]
    fn parse_empty_key_skipped() {
        let json = serde_json::json!([
            { "key": "", "status": 1 },
            { "key": "sk-real", "status": 1 },
        ]);
        let entries = parse_token_entries(&json);
        assert_eq!(
            select_first_usable_key(&entries),
            Some("sk-real".to_string())
        );
    }

    #[test]
    fn parse_newapi_fork_data_data() {
        // Some New-API forks use { "data": { "data": [...] } } instead of "items"
        let json = serde_json::json!({
            "success": true,
            "message": "",
            "data": {
                "data": [
                    { "id": 600, "key": "sk-fork1", "status": 1, "name": "KEY" },
                    { "id": 601, "key": "sk-fork2", "status": 2, "name": "KEY2" },
                ],
                "page": 1,
                "size": 100,
                "total_count": 2
            }
        });
        let entries = parse_token_entries(&json);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            select_first_usable_key(&entries),
            Some("sk-fork1".to_string())
        );
    }

    #[test]
    fn build_user_id_headers_contains_all() {
        let headers = build_user_id_headers(42);
        assert_eq!(headers.get("New-API-User").unwrap(), "42");
        assert_eq!(headers.get("Veloera-User").unwrap(), "42");
        assert_eq!(headers.get("voapi-user").unwrap(), "42");
        assert_eq!(headers.get("User-id").unwrap(), "42");
        assert_eq!(headers.get("Rix-Api-User").unwrap(), "42");
        assert_eq!(headers.get("neo-api-user").unwrap(), "42");
    }
}
