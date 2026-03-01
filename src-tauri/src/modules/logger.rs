use chrono;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use std::fs;
use std::path::PathBuf;

// ============================================================================
// Custom local timezone formatter
// ============================================================================

struct LocalTimer;

impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(
        &self,
        w: &mut tracing_subscriber::fmt::format::Writer<'_>,
    ) -> std::fmt::Result {
        let now = chrono::Local::now();
        write!(w, "{}", now.to_rfc3339())
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Get the log directory path, creating it if necessary.
///
/// Resolution order:
///   1. Current working directory (if cwd ends with `src-tauri`, use its
///      parent so logs land in the project root, not inside `src-tauri/`)
///   2. Executable's parent directory (fallback for packaged apps where cwd
///      may be `/` or `$HOME`)
pub fn get_log_dir() -> Result<PathBuf, String> {
    let base = std::env::current_dir()
        .ok()
        .map(|cwd| {
            // Tauri dev runs with cwd = src-tauri/; step up to project root.
            if cwd.file_name().and_then(|n| n.to_str()) == Some("src-tauri") {
                cwd.parent().map(|p| p.to_path_buf()).unwrap_or(cwd)
            } else {
                cwd
            }
        })
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        })
        .ok_or_else(|| "Cannot determine base directory for logs".to_string())?;

    let log_dir = base.join("logs");

    if !log_dir.exists() {
        fs::create_dir_all(&log_dir)
            .map_err(|e| format!("Failed to create log directory: {}", e))?;
    }

    Ok(log_dir)
}

/// Initialize the logging system (console + rolling file).
pub fn init_logger() {
    // Capture log macro output
    let _ = tracing_log::LogTracer::init();

    let log_dir = match get_log_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Failed to initialize log directory: {}", e);
            // Fallback: console-only logging
            let _ = tracing_subscriber::registry()
                .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
                .with(
                    fmt::Layer::new()
                        .with_target(false)
                        .with_level(true)
                        .with_timer(LocalTimer),
                )
                .try_init();
            return;
        }
    };

    // Fixed file name (no date suffix); old logs are cleaned up on startup.
    let file_appender = tracing_appender::rolling::never(&log_dir, "app.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // Console layer (local timezone)
    let console_layer = fmt::Layer::new()
        .with_target(false)
        .with_thread_ids(false)
        .with_level(true)
        .with_timer(LocalTimer);

    // File layer (no ANSI, local timezone)
    let file_layer = fmt::Layer::new()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_level(true)
        .with_timer(LocalTimer);

    // EnvFilter defaults to INFO
    let filter_layer =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // Initialize subscriber
    let _ = tracing_subscriber::registry()
        .with(filter_layer)
        .with(console_layer)
        .with(file_layer)
        .try_init();

    // Leak guard so the file writer lives until process exit
    std::mem::forget(_guard);

    info!("Log system initialized (Console + File persistence)");

    // Auto-cleanup logs older than 7 days
    if let Err(e) = cleanup_old_logs(7) {
        warn!("Failed to cleanup old logs: {}", e);
    }
}

/// Cleanup log files older than the specified number of days OR exceeding size limits.
pub fn cleanup_old_logs(days_to_keep: u64) -> Result<(), String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let log_dir = get_log_dir()?;
    if !log_dir.exists() {
        return Ok(());
    }

    const MAX_TOTAL_SIZE_BYTES: u64 = 1024 * 1024 * 1024; // 1GB
    const TARGET_SIZE_BYTES: u64 = 512 * 1024 * 1024; // 512MB

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("Failed to get system time: {}", e))?
        .as_secs();

    let cutoff_time = now.saturating_sub(days_to_keep * 24 * 60 * 60);

    let mut entries_info = Vec::new();
    let entries =
        fs::read_dir(&log_dir).map_err(|e| format!("Failed to read log directory: {}", e))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Ok(metadata) = fs::metadata(&path) {
            let modified = metadata.modified().unwrap_or(SystemTime::now());
            let modified_secs = modified
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let size = metadata.len();
            entries_info.push((path, size, modified_secs));
        }
    }

    let mut deleted_count = 0u64;
    let mut total_size_freed = 0u64;

    // Pass 1: delete files older than cutoff
    let mut remaining = Vec::new();
    for (path, size, modified_secs) in entries_info {
        if modified_secs < cutoff_time {
            if let Err(e) = fs::remove_file(&path) {
                warn!("Failed to delete old log file {:?}: {}", path, e);
                remaining.push((path, size, modified_secs));
            } else {
                deleted_count += 1;
                total_size_freed += size;
            }
        } else {
            remaining.push((path, size, modified_secs));
        }
    }

    // Pass 2: if total size exceeds limit, delete oldest first
    let mut current_total: u64 = remaining.iter().map(|(_, size, _)| *size).sum();
    if current_total > MAX_TOTAL_SIZE_BYTES {
        remaining.sort_by_key(|(_, _, modified)| *modified);
        for (path, size, _) in remaining {
            if current_total <= TARGET_SIZE_BYTES {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                deleted_count += 1;
                total_size_freed += size;
                current_total -= size;
            }
        }
    }

    if deleted_count > 0 {
        let size_mb = total_size_freed as f64 / 1024.0 / 1024.0;
        info!(
            "Log cleanup: deleted {} files, freed {:.2} MB",
            deleted_count, size_mb
        );
    }

    Ok(())
}

/// Clear all log files (delete them; the active writer will recreate on next write).
pub fn clear_logs() -> Result<(), String> {
    let log_dir = get_log_dir()?;
    if !log_dir.exists() {
        return Ok(());
    }

    let entries =
        fs::read_dir(&log_dir).map_err(|e| format!("Failed to read log directory: {}", e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let _ = fs::remove_file(&path);
        }
    }

    Ok(())
}

// Backward-compat convenience functions
pub fn log_info(message: &str) {
    info!("{}", message);
}

pub fn log_warn(message: &str) {
    warn!("{}", message);
}

pub fn log_error(message: &str) {
    error!("{}", message);
}
