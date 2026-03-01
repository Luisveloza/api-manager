use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

use crate::modules::config::get_data_dir;

static DB: once_cell::sync::Lazy<Mutex<Option<Connection>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpEntry {
    pub ip: String,
    pub reason: Option<String>,
    pub created_at: i64,
}

/// Initialize the security SQLite database.
pub fn init_db() -> Result<(), String> {
    let db_path = get_data_dir()
        .map_err(|e| format!("Failed to get data dir: {}", e))?
        .join("security.db");
    let conn = Connection::open(&db_path).map_err(|e| format!("Failed to open DB: {}", e))?;

    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "busy_timeout", "5000")
        .map_err(|e| e.to_string())?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ip_blacklist (
            ip TEXT PRIMARY KEY,
            reason TEXT,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
        );
        CREATE TABLE IF NOT EXISTS ip_whitelist (
            ip TEXT PRIMARY KEY,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
        );",
    )
    .map_err(|e| format!("Failed to create tables: {}", e))?;

    let mut guard = DB.lock().map_err(|e| e.to_string())?;
    *guard = Some(conn);

    Ok(())
}

fn with_db<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&Connection) -> Result<R, String>,
{
    let guard = DB.lock().map_err(|e| e.to_string())?;
    match guard.as_ref() {
        Some(conn) => f(conn),
        None => Err("Security database not initialized".to_string()),
    }
}

// ============================================================================
// Blacklist
// ============================================================================

pub fn add_to_blacklist(ip: &str, reason: &str) -> Result<(), String> {
    with_db(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO ip_blacklist (ip, reason) VALUES (?1, ?2)",
            params![ip, reason],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
}

pub fn remove_from_blacklist(ip: &str) -> Result<(), String> {
    with_db(|conn| {
        conn.execute("DELETE FROM ip_blacklist WHERE ip = ?1", params![ip])
            .map_err(|e| e.to_string())?;
        Ok(())
    })
}

pub fn is_blacklisted(ip: &str) -> bool {
    with_db(|conn| {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ip_blacklist WHERE ip = ?1",
                params![ip],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count > 0)
    })
    .unwrap_or(false)
}

pub fn get_blacklist() -> Result<Vec<IpEntry>, String> {
    with_db(|conn| {
        let mut stmt = conn
            .prepare("SELECT ip, reason, created_at FROM ip_blacklist ORDER BY created_at DESC")
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .query_map([], |row| {
                Ok(IpEntry {
                    ip: row.get(0)?,
                    reason: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })
            .map_err(|e| e.to_string())?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| e.to_string())?);
        }
        Ok(result)
    })
}

// ============================================================================
// Whitelist
// ============================================================================

pub fn add_to_whitelist(ip: &str) -> Result<(), String> {
    with_db(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO ip_whitelist (ip) VALUES (?1)",
            params![ip],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
}

pub fn remove_from_whitelist(ip: &str) -> Result<(), String> {
    with_db(|conn| {
        conn.execute("DELETE FROM ip_whitelist WHERE ip = ?1", params![ip])
            .map_err(|e| e.to_string())?;
        Ok(())
    })
}

pub fn is_whitelisted(ip: &str) -> bool {
    with_db(|conn| {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ip_whitelist WHERE ip = ?1",
                params![ip],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count > 0)
    })
    .unwrap_or(false)
}

pub fn get_whitelist() -> Result<Vec<IpEntry>, String> {
    with_db(|conn| {
        let mut stmt = conn
            .prepare("SELECT ip, created_at FROM ip_whitelist ORDER BY created_at DESC")
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .query_map([], |row| {
                Ok(IpEntry {
                    ip: row.get(0)?,
                    reason: None,
                    created_at: row.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| e.to_string())?);
        }
        Ok(result)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();

    fn init_test_db() {
        INIT.call_once(|| {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS ip_blacklist (
                    ip TEXT PRIMARY KEY,
                    reason TEXT,
                    created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
                );
                CREATE TABLE IF NOT EXISTS ip_whitelist (
                    ip TEXT PRIMARY KEY,
                    created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
                );",
            )
            .unwrap();

            let mut guard = DB.lock().unwrap();
            *guard = Some(conn);
        });
    }

    #[test]
    fn blacklist_crud() {
        init_test_db();

        assert!(!is_blacklisted("192.168.1.100"));

        add_to_blacklist("192.168.1.100", "abuse").unwrap();
        assert!(is_blacklisted("192.168.1.100"));

        let list = get_blacklist().unwrap();
        assert!(list.iter().any(|e| e.ip == "192.168.1.100"));

        remove_from_blacklist("192.168.1.100").unwrap();
        assert!(!is_blacklisted("192.168.1.100"));
    }

    #[test]
    fn whitelist_crud() {
        init_test_db();

        assert!(!is_whitelisted("10.0.0.1"));

        add_to_whitelist("10.0.0.1").unwrap();
        assert!(is_whitelisted("10.0.0.1"));

        let list = get_whitelist().unwrap();
        assert!(list.iter().any(|e| e.ip == "10.0.0.1"));

        remove_from_whitelist("10.0.0.1").unwrap();
        assert!(!is_whitelisted("10.0.0.1"));
    }
}
