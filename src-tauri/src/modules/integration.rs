/// System integration abstraction for Desktop vs Headless mode.
///
/// In APIManager (unlike AG), we don't need process control or DB injection.
/// This is a simplified version that only handles notifications and mode detection.
#[derive(Clone)]
pub enum SystemManager {
    Desktop(tauri::AppHandle),
    Headless,
}

impl SystemManager {
    /// Send a notification (desktop: system notification, headless: log)
    pub fn show_notification(&self, title: &str, body: &str) {
        match self {
            SystemManager::Desktop(_handle) => {
                tracing::info!("[Notification] {}: {}", title, body);
                // TODO: use tauri-plugin-notification when needed
            }
            SystemManager::Headless => {
                tracing::info!("[Log Notification] {}: {}", title, body);
            }
        }
    }

    /// Check if running in headless mode
    pub fn is_headless(&self) -> bool {
        matches!(self, Self::Headless)
    }
}
