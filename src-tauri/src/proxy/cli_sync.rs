use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub enum CliApp {
    Claude,
    Codex,
    Gemini,
    OpenCode,
    Droid,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct CliConfigFile {
    pub name: String,
    pub path: PathBuf,
}

/// Claude Code model role mapping.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ClaudeModelConfig {
    /// Root-level model alias (e.g. "opus", "sonnet")
    pub model: Option<String>,
    /// ANTHROPIC_MODEL env var
    pub primary_model: Option<String>,
    /// ANTHROPIC_DEFAULT_HAIKU_MODEL
    pub haiku_model: Option<String>,
    /// ANTHROPIC_DEFAULT_OPUS_MODEL
    pub opus_model: Option<String>,
    /// ANTHROPIC_DEFAULT_SONNET_MODEL
    pub sonnet_model: Option<String>,
    /// ANTHROPIC_REASONING_MODEL
    pub reasoning_model: Option<String>,
}

impl CliApp {
    pub fn as_str(&self) -> &'static str {
        match self {
            CliApp::Claude => "claude",
            CliApp::Codex => "codex",
            CliApp::Gemini => "gemini",
            CliApp::OpenCode => "opencode",
            CliApp::Droid => "droid",
        }
    }

    /// Files exposed to frontend (user-editable config).
    pub fn config_files(&self) -> Vec<CliConfigFile> {
        let home = match dirs::home_dir() {
            Some(p) => p,
            None => return vec![],
        };
        match self {
            CliApp::Claude => vec![
                // .claude.json is handled silently, not exposed to UI
                CliConfigFile {
                    name: "settings.json".to_string(),
                    path: home.join(".claude").join("settings.json"),
                },
            ],
            CliApp::Codex => vec![
                CliConfigFile {
                    name: "auth.json".to_string(),
                    path: home.join(".codex").join("auth.json"),
                },
                CliConfigFile {
                    name: "config.toml".to_string(),
                    path: home.join(".codex").join("config.toml"),
                },
            ],
            CliApp::Gemini => vec![
                CliConfigFile {
                    name: ".env".to_string(),
                    path: home.join(".gemini").join(".env"),
                },
                CliConfigFile {
                    name: "settings.json".to_string(),
                    path: home.join(".gemini").join("settings.json"),
                },
            ],
            CliApp::OpenCode => vec![CliConfigFile {
                name: "config.json".to_string(),
                path: home.join(".opencode").join("config.json"),
            }],
            CliApp::Droid => vec![CliConfigFile {
                name: "settings.json".to_string(),
                path: home.join(".factory").join("settings.json"),
            }],
        }
    }

    /// Internal-only file: ~/.claude.json (hasCompletedOnboarding).
    /// Not shown in UI, silently written during sync.
    fn claude_onboarding_file() -> Option<CliConfigFile> {
        dirs::home_dir().map(|home| CliConfigFile {
            name: ".claude.json".to_string(),
            path: home.join(".claude.json"),
        })
    }

    pub fn default_url(&self) -> &'static str {
        match self {
            CliApp::Claude => "https://api.anthropic.com",
            CliApp::Codex => "https://api.openai.com/v1",
            CliApp::Gemini => "https://generativelanguage.googleapis.com",
            CliApp::OpenCode => "https://api.openai.com/v1",
            CliApp::Droid => "https://api.anthropic.com",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CliStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub is_synced: bool,
    pub has_backup: bool,
    pub current_base_url: Option<String>,
    pub files: Vec<String>,
}

/// Detect whether a CLI is installed and get its version.
pub fn check_cli_installed(app: &CliApp) -> (bool, Option<String>) {
    let cmd = app.as_str();
    let mut executable_path = PathBuf::from(cmd);

    // 1. Use which/where first (respects PATH)
    let which_output = if cfg!(target_os = "windows") {
        let mut c = Command::new("where");
        c.arg(cmd);
        #[cfg(target_os = "windows")]
        c.creation_flags(CREATE_NO_WINDOW);
        c.output()
    } else {
        Command::new("which").arg(cmd).output()
    };

    let mut installed = match which_output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    };

    // macOS enhanced detection: Tauri process PATH may be incomplete
    if !installed && !cfg!(target_os = "windows") {
        let home = dirs::home_dir().unwrap_or_default();
        let mut common_paths = vec![
            home.join(".local/bin"),
            home.join(".bun/bin"),
            home.join(".bun/install/global/node_modules/.bin"),
            home.join(".npm-global/bin"),
            home.join(".volta/bin"),
            home.join("bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
        ];

        // Scan nvm node versions
        let nvm_base = home.join(".nvm/versions/node");
        if nvm_base.exists() {
            if let Ok(entries) = fs::read_dir(&nvm_base) {
                for entry in entries.flatten() {
                    let bin_path = entry.path().join("bin");
                    if bin_path.exists() {
                        common_paths.push(bin_path);
                    }
                }
            }
        }

        for path in common_paths {
            let full_path = path.join(cmd);
            if full_path.exists() {
                tracing::debug!("[CLI-Sync] Detected {} via path: {:?}", cmd, full_path);
                installed = true;
                executable_path = full_path;
                break;
            }
        }
    }

    if !installed {
        return (false, None);
    }

    // 2. Get version
    let mut ver_cmd = Command::new(&executable_path);
    ver_cmd.arg("--version");
    #[cfg(target_os = "windows")]
    ver_cmd.creation_flags(CREATE_NO_WINDOW);

    let version = match ver_cmd.output() {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let cleaned = s
                .split(|c: char| !c.is_numeric() && c != '.')
                .filter(|part| !part.is_empty())
                .last()
                .map(|p| p.trim())
                .unwrap_or(&s)
                .to_string();
            Some(cleaned)
        }
        _ => None,
    };

    (true, version)
}

/// Read current config and check sync status.
pub fn get_sync_status(app: &CliApp, proxy_url: &str) -> (bool, bool, Option<String>) {
    let files = app.config_files();
    if files.is_empty() {
        return (false, false, None);
    }

    let mut all_synced = true;
    let mut has_backup = false;
    let mut current_base_url = None;

    for file in &files {
        let backup_path = file
            .path
            .with_file_name(format!("{}.apimanager.bak", file.name));
        if backup_path.exists() {
            has_backup = true;
        }

        if !file.path.exists() {
            // Gemini settings.json is optional
            if app == &CliApp::Gemini && file.name == "settings.json" {
                continue;
            }
            all_synced = false;
            continue;
        }

        let content = match fs::read_to_string(&file.path) {
            Ok(c) => c,
            Err(_) => {
                all_synced = false;
                continue;
            }
        };

        match app {
            CliApp::Claude => {
                if file.name == "settings.json" {
                    let json: Value = serde_json::from_str(&content).unwrap_or_default();
                    let url = json
                        .get("env")
                        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
                        .and_then(|v| v.as_str());
                    if let Some(u) = url {
                        current_base_url = Some(u.to_string());
                        if u.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
            CliApp::Codex => {
                if file.name == "config.toml" {
                    let re =
                        regex::Regex::new(r#"(?m)^\s*base_url\s*=\s*['"]([^'"]+)['"]"#).unwrap();
                    if let Some(caps) = re.captures(&content) {
                        let url = &caps[1];
                        current_base_url = Some(url.to_string());
                        if url.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
            CliApp::Gemini => {
                if file.name == ".env" {
                    let re =
                        regex::Regex::new(r#"(?m)^GOOGLE_GEMINI_BASE_URL=(.*)$"#).unwrap();
                    if let Some(caps) = re.captures(&content) {
                        let url = caps[1].trim();
                        current_base_url = Some(url.to_string());
                        if url.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
            CliApp::OpenCode => {
                if file.name == "config.json" {
                    let json: Value = serde_json::from_str(&content).unwrap_or_default();
                    let url = json
                        .get("providers")
                        .and_then(|p| p.get("openai"))
                        .and_then(|o| o.get("baseURL"))
                        .and_then(|v| v.as_str());
                    if let Some(u) = url {
                        current_base_url = Some(u.to_string());
                        if u.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
            CliApp::Droid => {
                if file.name == "settings.json" {
                    let json: Value = serde_json::from_str(&content).unwrap_or_default();
                    let url = json.get("baseUrl").and_then(|v| v.as_str());
                    if let Some(u) = url {
                        current_base_url = Some(u.to_string());
                        if u.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
        }
    }

    (all_synced, has_backup, current_base_url)
}

/// Execute sync: backup existing config then write new values.
pub fn sync_config(
    app: &CliApp,
    proxy_url: &str,
    api_key: &str,
    model: Option<&str>,
    claude_models: Option<&ClaudeModelConfig>,
) -> Result<(), String> {
    // For Claude: silently write .claude.json (hasCompletedOnboarding)
    if app == &CliApp::Claude {
        if let Some(onboarding_file) = CliApp::claude_onboarding_file() {
            if let Some(parent) = onboarding_file.path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let content = if onboarding_file.path.exists() {
                fs::read_to_string(&onboarding_file.path).unwrap_or_default()
            } else {
                String::new()
            };
            let mut json: Value =
                serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = json.as_object_mut() {
                obj.insert("hasCompletedOnboarding".to_string(), Value::Bool(true));
            }
            let new_content = serde_json::to_string_pretty(&json).unwrap();
            let tmp_path = onboarding_file.path.with_extension("tmp");
            let _ = fs::write(&tmp_path, &new_content);
            let _ = fs::rename(&tmp_path, &onboarding_file.path);
        }
    }

    let files = app.config_files();

    for file in &files {
        if let Some(parent) = file.path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Cannot create directory: {}", e))?;
        }

        // Auto-backup: first sync saves .apimanager.bak, subsequent syncs don't overwrite it
        if file.path.exists() {
            let backup_path = file
                .path
                .with_file_name(format!("{}.apimanager.bak", file.name));
            if !backup_path.exists() {
                if let Err(e) = fs::copy(&file.path, &backup_path) {
                    tracing::warn!("Failed to create backup for {}: {}", file.name, e);
                } else {
                    tracing::info!("Created backup: {:?}", backup_path);
                }
            }
        }

        let mut content = if file.path.exists() {
            fs::read_to_string(&file.path).unwrap_or_default()
        } else {
            String::new()
        };

        match app {
            CliApp::Claude => {
                if file.name == "settings.json" {
                    let mut json: Value =
                        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                    if json.as_object().is_none() {
                        json = serde_json::json!({});
                    }
                    let env = json
                        .as_object_mut()
                        .unwrap()
                        .entry("env")
                        .or_insert(serde_json::json!({}));
                    if let Some(env_obj) = env.as_object_mut() {
                        env_obj.insert(
                            "ANTHROPIC_BASE_URL".to_string(),
                            Value::String(proxy_url.to_string()),
                        );
                        if !api_key.is_empty() {
                            env_obj.insert(
                                "ANTHROPIC_API_KEY".to_string(),
                                Value::String(api_key.to_string()),
                            );
                            // Remove conflicting auth token
                            env_obj.remove("ANTHROPIC_AUTH_TOKEN");
                        } else {
                            env_obj.remove("ANTHROPIC_API_KEY");
                        }

                        // Write Claude model role mappings
                        if let Some(cm) = claude_models {
                            set_or_remove(env_obj, "ANTHROPIC_MODEL", cm.primary_model.as_deref());
                            set_or_remove(env_obj, "ANTHROPIC_DEFAULT_HAIKU_MODEL", cm.haiku_model.as_deref());
                            set_or_remove(env_obj, "ANTHROPIC_DEFAULT_OPUS_MODEL", cm.opus_model.as_deref());
                            set_or_remove(env_obj, "ANTHROPIC_DEFAULT_SONNET_MODEL", cm.sonnet_model.as_deref());
                            set_or_remove(env_obj, "ANTHROPIC_REASONING_MODEL", cm.reasoning_model.as_deref());
                        } else {
                            // No model config provided: clean up model env vars
                            env_obj.remove("ANTHROPIC_MODEL");
                            env_obj.remove("ANTHROPIC_DEFAULT_HAIKU_MODEL");
                            env_obj.remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
                            env_obj.remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
                            env_obj.remove("ANTHROPIC_REASONING_MODEL");
                        }
                    }

                    // Root-level model alias
                    if let Some(cm) = claude_models {
                        if let Some(ref m) = cm.model {
                            json.as_object_mut()
                                .unwrap()
                                .insert("model".to_string(), Value::String(m.clone()));
                        }
                    }
                    content = serde_json::to_string_pretty(&json).unwrap();
                }
            }
            CliApp::Codex => {
                if file.name == "auth.json" {
                    let mut json: Value =
                        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                    if let Some(obj) = json.as_object_mut() {
                        obj.insert(
                            "OPENAI_API_KEY".to_string(),
                            Value::String(api_key.to_string()),
                        );
                        obj.insert(
                            "OPENAI_BASE_URL".to_string(),
                            Value::String(proxy_url.to_string()),
                        );
                    }
                    content = serde_json::to_string_pretty(&json).unwrap();
                } else if file.name == "config.toml" {
                    use toml_edit::{value, DocumentMut};
                    let mut doc = content
                        .parse::<DocumentMut>()
                        .unwrap_or_else(|_| DocumentMut::new());

                    let providers = doc
                        .entry("model_providers")
                        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                    if let Some(p_table) = providers.as_table_mut() {
                        let custom = p_table
                            .entry("custom")
                            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                        if let Some(c_table) = custom.as_table_mut() {
                            c_table.insert("name", value("custom"));
                            c_table.insert("wire_api", value("responses"));
                            c_table.insert("requires_openai_auth", value(true));
                            c_table.insert("base_url", value(proxy_url));
                            if let Some(m) = model {
                                c_table.insert("model", value(m));
                            }
                        }
                    }
                    doc.insert("model_provider", value("custom"));
                    if let Some(m) = model {
                        doc.insert("model", value(m));
                    }
                    doc.remove("openai_api_key");
                    doc.remove("openai_base_url");
                    content = doc.to_string();
                }
            }
            CliApp::Gemini => {
                if file.name == ".env" {
                    let mut lines: Vec<String> =
                        content.lines().map(|s| s.to_string()).collect();
                    let mut found_url = false;
                    let mut found_key = false;
                    for line in lines.iter_mut() {
                        if line.starts_with("GOOGLE_GEMINI_BASE_URL=") {
                            *line = format!("GOOGLE_GEMINI_BASE_URL={}", proxy_url);
                            found_url = true;
                        } else if line.trim().starts_with("GEMINI_API_KEY=") {
                            *line = format!("GEMINI_API_KEY={}", api_key);
                            found_key = true;
                        }
                    }
                    if !found_url {
                        lines.push(format!("GOOGLE_GEMINI_BASE_URL={}", proxy_url));
                    }
                    if !found_key {
                        lines.push(format!("GEMINI_API_KEY={}", api_key));
                    }
                    if let Some(m) = model {
                        let mut found_model = false;
                        for line in lines.iter_mut() {
                            if line.starts_with("GOOGLE_GEMINI_MODEL=") {
                                *line = format!("GOOGLE_GEMINI_MODEL={}", m);
                                found_model = true;
                            }
                        }
                        if !found_model {
                            lines.push(format!("GOOGLE_GEMINI_MODEL={}", m));
                        }
                    }
                    content = lines.join("\n");
                } else if file.name == "settings.json" {
                    let mut json: Value =
                        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                    if json.as_object().is_none() {
                        json = serde_json::json!({});
                    }
                    let sec = json
                        .as_object_mut()
                        .unwrap()
                        .entry("security")
                        .or_insert(serde_json::json!({}));
                    let auth = sec
                        .as_object_mut()
                        .unwrap()
                        .entry("auth")
                        .or_insert(serde_json::json!({}));
                    if let Some(auth_obj) = auth.as_object_mut() {
                        auth_obj.insert(
                            "selectedType".to_string(),
                            Value::String("gemini-api-key".to_string()),
                        );
                    }
                    content = serde_json::to_string_pretty(&json).unwrap();
                }
            }
            CliApp::OpenCode => {
                if file.name == "config.json" {
                    let mut json: Value =
                        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                    if json.as_object().is_none() {
                        json = serde_json::json!({});
                    }
                    let providers = json
                        .as_object_mut()
                        .unwrap()
                        .entry("providers")
                        .or_insert(serde_json::json!({}));
                    let openai = providers
                        .as_object_mut()
                        .unwrap()
                        .entry("openai")
                        .or_insert(serde_json::json!({}));
                    if let Some(openai_obj) = openai.as_object_mut() {
                        openai_obj.insert(
                            "baseURL".to_string(),
                            Value::String(proxy_url.to_string()),
                        );
                        if !api_key.is_empty() {
                            openai_obj.insert(
                                "apiKey".to_string(),
                                Value::String(api_key.to_string()),
                            );
                        }
                    }
                    content = serde_json::to_string_pretty(&json).unwrap();
                }
            }
            CliApp::Droid => {
                if file.name == "settings.json" {
                    let mut json: Value =
                        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                    if json.as_object().is_none() {
                        json = serde_json::json!({});
                    }
                    if let Some(obj) = json.as_object_mut() {
                        obj.insert(
                            "baseUrl".to_string(),
                            Value::String(proxy_url.to_string()),
                        );
                        if !api_key.is_empty() {
                            obj.insert(
                                "apiKey".to_string(),
                                Value::String(api_key.to_string()),
                            );
                        }
                    }
                    content = serde_json::to_string_pretty(&json).unwrap();
                }
            }
        }

        // Atomic write via tmp + rename
        let tmp_path = file.path.with_extension("tmp");
        fs::write(&tmp_path, &content)
            .map_err(|e| format!("Failed to write temp file: {}", e))?;
        fs::rename(&tmp_path, &file.path)
            .map_err(|e| format!("Failed to rename config file: {}", e))?;
    }

    Ok(())
}

/// Helper: set env var if value is Some, remove if None.
fn set_or_remove(obj: &mut serde_json::Map<String, Value>, key: &str, value: Option<&str>) {
    match value {
        Some(v) if !v.is_empty() => {
            obj.insert(key.to_string(), Value::String(v.to_string()));
        }
        _ => {
            obj.remove(key);
        }
    }
}

/// Generate config content for preview without writing.
pub fn generate_config_content(
    app: &CliApp,
    proxy_url: &str,
    api_key: &str,
    claude_models: Option<&ClaudeModelConfig>,
    model: Option<&str>,
    file_name: &str,
) -> Result<String, String> {
    let files = app.config_files();
    let file = files
        .into_iter()
        .find(|f| f.name == file_name)
        .ok_or_else(|| format!("File not found: {}", file_name))?;

    let content = if file.path.exists() {
        fs::read_to_string(&file.path).unwrap_or_default()
    } else {
        String::new()
    };

    let result = match app {
        CliApp::Claude => {
            if file_name == "settings.json" {
                let mut json: Value =
                    serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                if json.as_object().is_none() {
                    json = serde_json::json!({});
                }
                let env = json
                    .as_object_mut()
                    .unwrap()
                    .entry("env")
                    .or_insert(serde_json::json!({}));
                if let Some(env_obj) = env.as_object_mut() {
                    env_obj.insert(
                        "ANTHROPIC_BASE_URL".to_string(),
                        Value::String(proxy_url.to_string()),
                    );
                    if !api_key.is_empty() {
                        env_obj.insert(
                            "ANTHROPIC_API_KEY".to_string(),
                            Value::String(api_key.to_string()),
                        );
                        env_obj.remove("ANTHROPIC_AUTH_TOKEN");
                    }
                    if let Some(cm) = claude_models {
                        set_or_remove(env_obj, "ANTHROPIC_MODEL", cm.primary_model.as_deref());
                        set_or_remove(env_obj, "ANTHROPIC_DEFAULT_HAIKU_MODEL", cm.haiku_model.as_deref());
                        set_or_remove(env_obj, "ANTHROPIC_DEFAULT_OPUS_MODEL", cm.opus_model.as_deref());
                        set_or_remove(env_obj, "ANTHROPIC_DEFAULT_SONNET_MODEL", cm.sonnet_model.as_deref());
                        set_or_remove(env_obj, "ANTHROPIC_REASONING_MODEL", cm.reasoning_model.as_deref());
                    }
                }
                if let Some(cm) = claude_models {
                    if let Some(ref m) = cm.model {
                        json.as_object_mut()
                            .unwrap()
                            .insert("model".to_string(), Value::String(m.clone()));
                    }
                }
                serde_json::to_string_pretty(&json).unwrap()
            } else {
                content
            }
        }
        CliApp::Codex => {
            if file_name == "auth.json" {
                let mut json: Value =
                    serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                if let Some(obj) = json.as_object_mut() {
                    obj.insert(
                        "OPENAI_API_KEY".to_string(),
                        Value::String(api_key.to_string()),
                    );
                    obj.insert(
                        "OPENAI_BASE_URL".to_string(),
                        Value::String(proxy_url.to_string()),
                    );
                }
                serde_json::to_string_pretty(&json).unwrap()
            } else if file_name == "config.toml" {
                use toml_edit::{value, DocumentMut};
                let mut doc = content
                    .parse::<DocumentMut>()
                    .unwrap_or_else(|_| DocumentMut::new());
                let providers = doc
                    .entry("model_providers")
                    .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                if let Some(p_table) = providers.as_table_mut() {
                    let custom = p_table
                        .entry("custom")
                        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                    if let Some(c_table) = custom.as_table_mut() {
                        c_table.insert("name", value("custom"));
                        c_table.insert("wire_api", value("responses"));
                        c_table.insert("requires_openai_auth", value(true));
                        c_table.insert("base_url", value(proxy_url));
                        if let Some(m) = model {
                            c_table.insert("model", value(m));
                        }
                    }
                }
                doc.insert("model_provider", value("custom"));
                if let Some(m) = model {
                    doc.insert("model", value(m));
                }
                doc.to_string()
            } else {
                content
            }
        }
        CliApp::Gemini => {
            if file_name == ".env" {
                let mut lines: Vec<String> =
                    content.lines().map(|s| s.to_string()).collect();
                let mut found_url = false;
                let mut found_key = false;
                for line in lines.iter_mut() {
                    if line.starts_with("GOOGLE_GEMINI_BASE_URL=") {
                        *line = format!("GOOGLE_GEMINI_BASE_URL={}", proxy_url);
                        found_url = true;
                    } else if line.trim().starts_with("GEMINI_API_KEY=") {
                        *line = format!("GEMINI_API_KEY={}", api_key);
                        found_key = true;
                    }
                }
                if !found_url {
                    lines.push(format!("GOOGLE_GEMINI_BASE_URL={}", proxy_url));
                }
                if !found_key {
                    lines.push(format!("GEMINI_API_KEY={}", api_key));
                }
                lines.join("\n")
            } else if file_name == "settings.json" {
                let mut json: Value =
                    serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                if json.as_object().is_none() {
                    json = serde_json::json!({});
                }
                let sec = json
                    .as_object_mut()
                    .unwrap()
                    .entry("security")
                    .or_insert(serde_json::json!({}));
                let auth = sec
                    .as_object_mut()
                    .unwrap()
                    .entry("auth")
                    .or_insert(serde_json::json!({}));
                if let Some(auth_obj) = auth.as_object_mut() {
                    auth_obj.insert(
                        "selectedType".to_string(),
                        Value::String("gemini-api-key".to_string()),
                    );
                }
                serde_json::to_string_pretty(&json).unwrap()
            } else {
                content
            }
        }
        CliApp::OpenCode => {
            if file_name == "config.json" {
                let mut json: Value =
                    serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                if json.as_object().is_none() {
                    json = serde_json::json!({});
                }
                let providers = json
                    .as_object_mut()
                    .unwrap()
                    .entry("providers")
                    .or_insert(serde_json::json!({}));
                let openai = providers
                    .as_object_mut()
                    .unwrap()
                    .entry("openai")
                    .or_insert(serde_json::json!({}));
                if let Some(openai_obj) = openai.as_object_mut() {
                    openai_obj.insert(
                        "baseURL".to_string(),
                        Value::String(proxy_url.to_string()),
                    );
                    if !api_key.is_empty() {
                        openai_obj.insert(
                            "apiKey".to_string(),
                            Value::String(api_key.to_string()),
                        );
                    }
                }
                serde_json::to_string_pretty(&json).unwrap()
            } else {
                content
            }
        }
        CliApp::Droid => {
            if file_name == "settings.json" {
                let mut json: Value =
                    serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                if json.as_object().is_none() {
                    json = serde_json::json!({});
                }
                if let Some(obj) = json.as_object_mut() {
                    obj.insert(
                        "baseUrl".to_string(),
                        Value::String(proxy_url.to_string()),
                    );
                    if !api_key.is_empty() {
                        obj.insert(
                            "apiKey".to_string(),
                            Value::String(api_key.to_string()),
                        );
                    }
                }
                serde_json::to_string_pretty(&json).unwrap()
            } else {
                content
            }
        }
    };

    Ok(result)
}

// ============================================================================
// Tauri Commands
// ============================================================================

#[tauri::command(rename_all = "camelCase")]
pub async fn get_cli_sync_status(
    app_type: CliApp,
    proxy_url: String,
) -> Result<CliStatus, String> {
    let (installed, version) = check_cli_installed(&app_type);
    let (is_synced, has_backup, current_base_url) = if installed {
        get_sync_status(&app_type, &proxy_url)
    } else {
        (false, false, None)
    };

    Ok(CliStatus {
        installed,
        version,
        is_synced,
        has_backup,
        current_base_url,
        files: app_type
            .config_files()
            .into_iter()
            .map(|f| f.name)
            .collect(),
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn execute_cli_sync(
    app_type: CliApp,
    proxy_url: String,
    api_key: String,
    model: Option<String>,
    claude_models: Option<ClaudeModelConfig>,
) -> Result<(), String> {
    sync_config(&app_type, &proxy_url, &api_key, model.as_deref(), claude_models.as_ref())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn execute_cli_restore(app_type: CliApp) -> Result<(), String> {
    let files = app_type.config_files();
    let mut restored_count = 0;

    // Also restore .claude.json if it's Claude
    if app_type == CliApp::Claude {
        if let Some(onboarding_file) = CliApp::claude_onboarding_file() {
            let backup_path = onboarding_file
                .path
                .with_file_name(format!("{}.apimanager.bak", onboarding_file.name));
            if backup_path.exists() {
                if let Err(e) = fs::rename(&backup_path, &onboarding_file.path) {
                    tracing::warn!("Failed to restore .claude.json backup: {}", e);
                } else {
                    restored_count += 1;
                }
            }
        }
    }

    for file in &files {
        let backup_path = file
            .path
            .with_file_name(format!("{}.apimanager.bak", file.name));
        if backup_path.exists() {
            if let Err(e) = fs::rename(&backup_path, &file.path) {
                return Err(format!("Restore backup failed {}: {}", file.name, e));
            }
            restored_count += 1;
        }
    }

    if restored_count > 0 {
        return Ok(());
    }

    // No backup: restore to default URLs
    let default_url = app_type.default_url();
    sync_config(&app_type, default_url, "", None, None)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_cli_config_content(
    app_type: CliApp,
    file_name: Option<String>,
) -> Result<String, String> {
    let files = app_type.config_files();
    let file = if let Some(name) = file_name {
        files
            .into_iter()
            .find(|f| f.name == name)
            .ok_or("File not found".to_string())?
    } else {
        files.into_iter().next().ok_or("No config file".to_string())?
    };

    if !file.path.exists() {
        return Err("Config file does not exist".to_string());
    }
    fs::read_to_string(&file.path).map_err(|e| format!("Failed to read config: {}", e))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn generate_cli_config(
    app_type: CliApp,
    proxy_url: String,
    api_key: String,
    model: Option<String>,
    claude_models: Option<ClaudeModelConfig>,
    file_name: String,
) -> Result<String, String> {
    generate_config_content(
        &app_type,
        &proxy_url,
        &api_key,
        claude_models.as_ref(),
        model.as_deref(),
        &file_name,
    )
}

#[tauri::command(rename_all = "camelCase")]
pub async fn write_cli_config(
    app_type: CliApp,
    file_name: String,
    content: String,
) -> Result<(), String> {
    let files = app_type.config_files();
    let file = files
        .into_iter()
        .find(|f| f.name == file_name)
        .ok_or_else(|| format!("File not found: {}", file_name))?;

    if let Some(parent) = file.path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create directory: {}", e))?;
    }

    // For Claude write_cli_config on settings.json, also silently write .claude.json
    if app_type == CliApp::Claude && file_name == "settings.json" {
        if let Some(onboarding_file) = CliApp::claude_onboarding_file() {
            if let Some(parent) = onboarding_file.path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let ob_content = if onboarding_file.path.exists() {
                fs::read_to_string(&onboarding_file.path).unwrap_or_default()
            } else {
                String::new()
            };
            let mut json: Value =
                serde_json::from_str(&ob_content).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = json.as_object_mut() {
                obj.insert("hasCompletedOnboarding".to_string(), Value::Bool(true));
            }
            let new_content = serde_json::to_string_pretty(&json).unwrap();
            let tmp = onboarding_file.path.with_extension("tmp");
            let _ = fs::write(&tmp, &new_content);
            let _ = fs::rename(&tmp, &onboarding_file.path);
        }
    }

    // Atomic write via tmp + rename
    let tmp_path = file.path.with_extension("tmp");
    fs::write(&tmp_path, &content)
        .map_err(|e| format!("Failed to write temp file: {}", e))?;
    fs::rename(&tmp_path, &file.path)
        .map_err(|e| format!("Failed to rename config file: {}", e))?;

    Ok(())
}

// ============================================================================
// CLI Compatibility Probe
// ============================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CliProbeResult {
    pub cli_name: String,
    pub config_found: bool,
    pub config_valid: bool,
    pub proxy_reachable: bool,
    pub auth_ok: bool,
    pub model_available: bool,
    pub response_valid: bool,
    pub error: Option<String>,
    pub latency_ms: u64,
}

/// Probe a CLI's configuration and test connectivity to the local proxy.
pub async fn probe_cli(
    app: &CliApp,
    proxy_port: u16,
    api_key: &str,
) -> CliProbeResult {
    let cli_name = app.as_str().to_string();
    let mut result = CliProbeResult {
        cli_name: cli_name.clone(),
        config_found: false,
        config_valid: false,
        proxy_reachable: false,
        auth_ok: false,
        model_available: false,
        response_valid: false,
        error: None,
        latency_ms: 0,
    };

    // 1. Check config files exist
    let files = app.config_files();
    if files.is_empty() {
        result.error = Some("No config files defined".to_string());
        return result;
    }
    result.config_found = files.iter().any(|f| f.path.exists());

    // 2. Check config points to local proxy
    let proxy_url = format!("http://127.0.0.1:{}", proxy_port);
    let (is_synced, _, _) = get_sync_status(app, &proxy_url);
    result.config_valid = result.config_found && is_synced;

    // 3. Test proxy reachability (health endpoint)
    let health_url = format!("{}/health", proxy_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    match client.get(&health_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            result.proxy_reachable = true;
        }
        Ok(resp) => {
            result.error = Some(format!("Health check returned {}", resp.status()));
            return result;
        }
        Err(e) => {
            result.error = Some(format!("Proxy unreachable: {}", e));
            return result;
        }
    }

    // 4. Test auth + model availability + response validity via a minimal request
    let test_url = format!("{}/v1/chat/completions", proxy_url);
    let test_body = serde_json::json!({
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "Say OK"}],
        "max_tokens": 5,
        "stream": false,
    });

    let start = std::time::Instant::now();
    match client
        .post(&test_url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&test_body)
        .send()
        .await
    {
        Ok(resp) => {
            result.latency_ms = start.elapsed().as_millis() as u64;
            let status = resp.status().as_u16();

            if status == 401 || status == 403 {
                result.error = Some("Authentication failed".to_string());
                return result;
            }
            result.auth_ok = true;

            if status == 404 {
                result.error = Some("Model not found".to_string());
                return result;
            }

            if (200..300).contains(&status) {
                result.model_available = true;
                // Try to parse response
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    // OpenAI format: choices[0].message.content
                    if body.get("choices").is_some() || body.get("content").is_some() {
                        result.response_valid = true;
                    } else {
                        result.error = Some("Unexpected response format".to_string());
                    }
                } else {
                    result.error = Some("Failed to parse response JSON".to_string());
                }
            } else if status == 503 {
                result.error = Some("No available accounts".to_string());
            } else {
                result.error = Some(format!("Upstream returned {}", status));
                result.model_available = true; // model routing worked, just upstream error
            }
        }
        Err(e) => {
            result.latency_ms = start.elapsed().as_millis() as u64;
            result.error = Some(format!("Request failed: {}", e));
        }
    }

    result
}

#[tauri::command(rename_all = "camelCase")]
pub async fn probe_cli_compatibility(
    app_type: CliApp,
    proxy_port: u16,
    api_key: String,
) -> Result<CliProbeResult, String> {
    Ok(probe_cli(&app_type, proxy_port, &api_key).await)
}
