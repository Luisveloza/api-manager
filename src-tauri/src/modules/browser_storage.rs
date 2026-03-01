//! Read All API Hub browser extension data from Chrome's LevelDB storage.
//!
//! Chrome stores `browser.storage.local` data for each extension in a LevelDB
//! database at `<Profile>/Local Extension Settings/<extension-id>/`.
//!
//! Because Chrome holds a file lock on the DB while running, we **copy** the
//! entire directory to a temp location, remove the LOCK file, and then open it
//! with `rusty-leveldb`.

use std::path::{Path, PathBuf};

use rusty_leveldb::{Options, DB};

/// Known Chrome Web Store extension IDs for All API Hub.
const KNOWN_EXTENSION_IDS: &[&str] = &[
    "hnmbbaagobbadojmjkeilcgbnpdfifmk",
    "lapnciffpekdengooeolaienkeoilfeo",
];

/// The storage key used by @plasmohq/storage for the accounts data.
const STORAGE_KEY: &str = "site_accounts";

/// Information about a discovered extension directory.
#[derive(Debug, Clone)]
pub struct ExtensionInfo {
    pub profile_name: String,
    pub extension_id: String,
    pub path: PathBuf,
}

// ---------------------------------------------------------------------------
// Platform-specific Chrome base directory
// ---------------------------------------------------------------------------

/// Return the Chrome user-data base directory for the current platform.
fn get_chrome_base_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|h| {
            h.join("Library")
                .join("Application Support")
                .join("Google")
                .join("Chrome")
        })
    }

    #[cfg(target_os = "linux")]
    {
        dirs::home_dir().map(|h| h.join(".config").join("google-chrome"))
    }

    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir().map(|d| d.join("Google").join("Chrome").join("User Data"))
    }
}

// ---------------------------------------------------------------------------
// Profile discovery
// ---------------------------------------------------------------------------

/// List all Chrome profile directories that contain a `Local Extension Settings` folder.
fn list_chrome_profiles(base: &Path) -> Vec<(String, PathBuf)> {
    let mut profiles = Vec::new();

    let Ok(entries) = std::fs::read_dir(base) else {
        return profiles;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Chrome profiles are "Default", "Profile 1", "Profile 2", etc.
        if name == "Default" || name.starts_with("Profile ") {
            let ext_settings = entry.path().join("Local Extension Settings");
            if ext_settings.is_dir() {
                profiles.push((name, ext_settings));
            }
        }
    }

    profiles
}

// ---------------------------------------------------------------------------
// Extension discovery
// ---------------------------------------------------------------------------

/// Discover All API Hub extension directories across all Chrome profiles.
///
/// Strategy:
/// 1. Look for the known Web Store extension ID first (fast path).
/// 2. Fall back to scanning all extension directories and checking for
///    the `site_accounts` key (slow path, for sideloaded / dev installs).
pub fn discover_extension_dirs() -> Vec<ExtensionInfo> {
    let base = match get_chrome_base_dir() {
        Some(b) if b.is_dir() => b,
        _ => {
            tracing::info!("Chrome base directory not found");
            return Vec::new();
        }
    };

    let profiles = list_chrome_profiles(&base);
    if profiles.is_empty() {
        tracing::info!("No Chrome profiles found in {:?}", base);
        return Vec::new();
    }

    let mut results = Vec::new();

    // --- Fast path: known extension IDs ---
    for (profile_name, ext_settings_dir) in &profiles {
        for &ext_id in KNOWN_EXTENSION_IDS {
            let candidate = ext_settings_dir.join(ext_id);
            if candidate.is_dir() {
                tracing::info!(
                    profile = %profile_name,
                    path = %candidate.display(),
                    "Found extension via known ID"
                );
                results.push(ExtensionInfo {
                    profile_name: profile_name.clone(),
                    extension_id: ext_id.to_string(),
                    path: candidate,
                });
            }
        }
    }

    if !results.is_empty() {
        return results;
    }

    // --- Slow path: scan all extensions for the storage key ---
    tracing::info!("Known extension ID not found, scanning all extensions...");

    for (profile_name, ext_settings_dir) in &profiles {
        let Ok(entries) = std::fs::read_dir(ext_settings_dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let ext_id = entry.file_name().to_string_lossy().to_string();
            let ext_path = entry.path();

            if !ext_path.is_dir() {
                continue;
            }

            // Quick check: does the directory look like a LevelDB database?
            if !ext_path.join("CURRENT").exists() {
                continue;
            }

            // Try to read the storage key from this extension
            match read_extension_storage(&ext_path) {
                Ok(data) if !data.is_empty() => {
                    tracing::info!(
                        profile = %profile_name,
                        ext_id = %ext_id,
                        "Found extension via scan (has site_accounts key)"
                    );
                    results.push(ExtensionInfo {
                        profile_name: profile_name.clone(),
                        extension_id: ext_id,
                        path: ext_path,
                    });
                }
                _ => {}
            }
        }
    }

    results
}

// ---------------------------------------------------------------------------
// LevelDB reading
// ---------------------------------------------------------------------------

/// Read the `site_accounts` value from a Chrome extension's LevelDB storage.
///
/// Steps:
/// 1. Copy the entire LevelDB directory to a temp location.
/// 2. Remove the LOCK file so we can open it without Chrome releasing its lock.
/// 3. Open with `rusty-leveldb`.
/// 4. Read the `site_accounts` key.
/// 5. Clean up the temp directory.
pub fn read_extension_storage(ext_dir: &Path) -> Result<String, String> {
    // 1. Create temp directory
    let temp_dir = std::env::temp_dir().join(format!(
        "apimanager-leveldb-{}",
        uuid::Uuid::new_v4().simple()
    ));

    copy_dir_all(ext_dir, &temp_dir)
        .map_err(|e| format!("Failed to copy LevelDB dir to temp: {}", e))?;

    // 2. Remove LOCK file
    let lock_file = temp_dir.join("LOCK");
    if lock_file.exists() {
        let _ = std::fs::remove_file(&lock_file);
    }

    // 3. Open LevelDB
    let opts = Options::default();
    let mut db = DB::open(&temp_dir, opts)
        .map_err(|e| format!("Failed to open LevelDB: {:?}", e))?;

    // 4. Read the key
    let raw_value = db
        .get(STORAGE_KEY.as_bytes())
        .ok_or_else(|| "Key 'site_accounts' not found in LevelDB".to_string())?;

    // 5. Clean up
    drop(db);
    let _ = std::fs::remove_dir_all(&temp_dir);

    // Convert bytes to string
    let value_str = String::from_utf8(raw_value.to_vec())
        .map_err(|e| format!("Invalid UTF-8 in LevelDB value: {}", e))?;

    // @plasmohq/storage wraps the value in an extra JSON.stringify, so we may
    // get a JSON string that is itself a stringified JSON object.  Unwrap one
    // layer if the outer value is a JSON string literal.
    let cleaned = unwrap_json_string(&value_str);

    // Validate that it looks like JSON (starts with '{' or '[')
    let trimmed = cleaned.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return Err(format!(
            "site_accounts value is not valid JSON (starts with {:?})",
            &trimmed[..trimmed.len().min(20)]
        ));
    }

    Ok(cleaned)
}

/// Top-level entry point: discover extensions and read accounts data.
pub fn read_accounts_from_browser() -> Result<String, String> {
    let dirs = discover_extension_dirs();

    if dirs.is_empty() {
        return Err(
            "All API Hub extension not found. Please ensure Chrome is installed and the extension is active."
                .to_string(),
        );
    }

    let mut last_error = String::new();

    for info in &dirs {
        tracing::info!(
            profile = %info.profile_name,
            ext_id = %info.extension_id,
            "Trying to read extension storage"
        );

        match read_extension_storage(&info.path) {
            Ok(data) => {
                tracing::info!(
                    profile = %info.profile_name,
                    data_len = data.len(),
                    "Successfully read extension storage"
                );
                return Ok(data);
            }
            Err(e) => {
                tracing::warn!(
                    profile = %info.profile_name,
                    error = %e,
                    "Failed to read extension storage, trying next"
                );
                last_error = e;
            }
        }
    }

    Err(format!(
        "Failed to read extension data from any profile. Last error: {}",
        last_error
    ))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Recursively copy a directory and all its contents.
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());

        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }

    Ok(())
}

/// If `s` is a JSON string literal (starts and ends with `"`), parse it to
/// unwrap one layer of escaping.  Otherwise return the original.
fn unwrap_json_string(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') {
        // Try to parse as a JSON string to unescape
        if let Ok(inner) = serde_json::from_str::<String>(trimmed) {
            return inner;
        }
    }
    s.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chrome_base_paths() {
        // Just verify the function doesn't panic
        let _base = get_chrome_base_dir();
    }

    #[test]
    fn test_unwrap_json_string_plain() {
        let input = r#"{"accounts": []}"#;
        assert_eq!(unwrap_json_string(input), input);
    }

    #[test]
    fn test_unwrap_json_string_wrapped() {
        // @plasmohq/storage wraps the value like: "{\"accounts\": []}"
        let inner = r#"{"accounts": []}"#;
        let wrapped = serde_json::to_string(inner).unwrap(); // produces "\"{ ... }\""
        let result = unwrap_json_string(&wrapped);
        assert_eq!(result, inner);
    }

    #[test]
    fn test_discover_with_mock_structure() {
        // Create a temporary directory structure mimicking Chrome
        let temp = std::env::temp_dir().join(format!(
            "apimanager-test-chrome-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let profile_dir = temp.join("Default").join("Local Extension Settings");
        let ext_dir = profile_dir.join(KNOWN_EXTENSION_IDS[0]);
        std::fs::create_dir_all(&ext_dir).unwrap();

        // Write a CURRENT file to make it look like LevelDB
        std::fs::write(ext_dir.join("CURRENT"), "MANIFEST-000001\n").unwrap();

        // Clean up
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_read_leveldb_basic() {
        // Create a real LevelDB database with site_accounts data
        let temp = std::env::temp_dir().join(format!(
            "apimanager-test-leveldb-{}",
            uuid::Uuid::new_v4().simple()
        ));

        let test_data = r#"{"configVersion":5,"accounts":[{"id":"test-1","site_name":"Test","site_url":"https://test.com","site_type":"new-api","account_info":{"id":1,"access_token":"sk-test","username":"test","quota":1000.0,"today_prompt_tokens":0,"today_completion_tokens":0,"today_quota_consumption":0.0,"today_requests_count":0,"today_income":0.0}}]}"#;

        // @plasmohq/storage wraps value in JSON.stringify
        let wrapped = serde_json::to_string(test_data).unwrap();

        // Write to a LevelDB
        {
            let opts = Options::default();
            let mut db = DB::open(&temp, opts).unwrap();
            db.put(STORAGE_KEY.as_bytes(), wrapped.as_bytes()).unwrap();
            db.flush().unwrap();
        }

        // Read it back
        let result = read_extension_storage(&temp).unwrap();

        // Should contain the accounts data
        assert!(result.contains("test-1"));
        assert!(result.contains("configVersion"));

        // Clean up
        let _ = std::fs::remove_dir_all(&temp);
    }
}
