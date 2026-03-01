pub mod commands;
pub mod constants;
pub mod error;
pub mod models;
pub mod modules;
pub mod proxy;

use commands::ProxyServiceState;
use tauri::Manager;

pub fn run() {
    // Initialize logger first
    modules::logger::init_logger();

    // Check for --headless mode
    let is_headless = std::env::args().any(|arg| arg == "--headless");

    if is_headless {
        run_headless();
        return;
    }

    // GUI mode: Tauri application
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(ProxyServiceState::new())
        .setup(|app| {
            let state = app.state::<ProxyServiceState>().inner().clone();
            tauri::async_runtime::spawn(async move {
                let config = modules::config::load_app_config();
                if config.proxy.api_key.is_empty() && config.proxy_accounts.is_empty() {
                    tracing::info!("No proxy config found, skipping auto-start");
                    return;
                }
                tracing::info!("Auto-starting proxy server on port {}", config.proxy.port);
                match proxy::server::start_server(&config.proxy, &config.proxy_accounts).await {
                    Ok(axum_server) => {
                        let monitor_ref = axum_server.monitor.clone();
                        let token_manager_ref = axum_server.token_manager.clone();
                        {
                            let mut server = state.server.lock().await;
                            server.set_server(axum_server);
                        }
                        {
                            let mut monitor = state.monitor.write().await;
                            *monitor = Some(monitor_ref);
                        }
                        {
                            let mut tm = state.token_manager.write().await;
                            *tm = Some(token_manager_ref);
                        }
                        tracing::info!("Proxy server auto-started successfully");
                    }
                    Err(e) => {
                        tracing::error!("Failed to auto-start proxy server: {}", e);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::import_backup,
            commands::import_backup_from_text,
            commands::detect_browser_extension,
            commands::sync_from_browser,
            commands::load_config,
            commands::refresh_api_keys,
            commands::save_config,
            commands::proxy_start,
            commands::proxy_stop,
            commands::get_proxy_status,
            commands::get_logs,
            commands::replay_request,
            commands::get_available_models,
            commands::get_proxy_stats,
            commands::validate_api_key,
            proxy::cli_sync::get_cli_sync_status,
            proxy::cli_sync::execute_cli_sync,
            proxy::cli_sync::execute_cli_restore,
            proxy::cli_sync::get_cli_config_content,
            proxy::cli_sync::generate_cli_config,
            proxy::cli_sync::write_cli_config,
            proxy::cli_sync::probe_cli_compatibility,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn run_headless() {
    tracing::info!("Starting APIManager in headless mode...");

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        // Load config
        let mut config = modules::config::load_app_config();

        // Environment variable overrides
        if let Ok(key) = std::env::var("ABV_API_KEY").or_else(|_| std::env::var("API_KEY")) {
            config.proxy.api_key = key;
        }
        if let Ok(pw) = std::env::var("ABV_WEB_PASSWORD").or_else(|_| std::env::var("WEB_PASSWORD"))
        {
            config.proxy.admin_password = Some(pw);
        }
        if let Ok(port) = std::env::var("PORT")
            .or_else(|_| std::env::var("ABV_PORT"))
            .and_then(|p| p.parse::<u16>().map_err(|_| std::env::VarError::NotPresent))
        {
            config.proxy.port = port;
        }
        if let Ok(mode) = std::env::var("ABV_AUTH_MODE").or_else(|_| std::env::var("AUTH_MODE")) {
            config.proxy.auth_mode = match mode.to_lowercase().as_str() {
                "off" => models::ProxyAuthMode::Off,
                "strict" => models::ProxyAuthMode::Strict,
                "all_except_health" => models::ProxyAuthMode::AllExceptHealth,
                _ => models::ProxyAuthMode::Auto,
            };
        }

        // Headless defaults
        if let Ok(val) = std::env::var("ABV_BIND_LOCAL_ONLY") {
            if val == "true" || val == "1" {
                config.proxy.allow_lan_access = false;
            }
        } else {
            config.proxy.allow_lan_access = true; // Docker default
        }

        tracing::info!(
            port = config.proxy.port,
            api_key = %mask_key(&config.proxy.api_key),
            auth_mode = ?config.proxy.auth_mode,
            lan_access = config.proxy.allow_lan_access,
            accounts = config.proxy_accounts.len(),
            "Headless configuration loaded"
        );

        // Start proxy server using AxumServer
        match proxy::server::start_server(&config.proxy, &config.proxy_accounts).await {
            Ok(mut server) => {
                tracing::info!(
                    "Proxy server running on {}:{}",
                    if config.proxy.allow_lan_access { "0.0.0.0" } else { "127.0.0.1" },
                    config.proxy.port
                );

                // Wait for Ctrl-C
                tokio::signal::ctrl_c()
                    .await
                    .expect("Failed to listen for ctrl-c");
                tracing::info!("Received Ctrl-C, shutting down...");
                server.stop().await;
            }
            Err(e) => {
                tracing::error!("Failed to start proxy server: {}", e);
                std::process::exit(1);
            }
        }
    });
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        "***".to_string()
    } else {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    }
}
