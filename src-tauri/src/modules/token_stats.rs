use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

use crate::modules::config::get_data_dir;

static DB: once_cell::sync::Lazy<Mutex<Option<Connection>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

/// Initialize the token stats SQLite database.
pub fn init_db() -> Result<(), String> {
    let db_path = get_data_dir()
        .map_err(|e| format!("Failed to get data dir: {}", e))?
        .join("token_stats.db");
    let conn = Connection::open(&db_path).map_err(|e| format!("Failed to open DB: {}", e))?;

    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "busy_timeout", "5000")
        .map_err(|e| e.to_string())?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS token_usage (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp INTEGER NOT NULL DEFAULT (strftime('%s','now')),
            account_email TEXT NOT NULL,
            model TEXT NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_token_usage_timestamp ON token_usage(timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_token_usage_account ON token_usage(account_email);
        CREATE INDEX IF NOT EXISTS idx_token_usage_model ON token_usage(model);",
    )
    .map_err(|e| format!("Failed to create table: {}", e))?;

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
        None => Err("Database not initialized".to_string()),
    }
}

/// Record a token usage entry.
pub fn record_usage(account: &str, model: &str, input: i32, output: i32) -> Result<(), String> {
    with_db(|conn| {
        conn.execute(
            "INSERT INTO token_usage (account_email, model, input_tokens, output_tokens) VALUES (?1, ?2, ?3, ?4)",
            params![account, model, input, output],
        )
        .map_err(|e| format!("Failed to insert: {}", e))?;
        Ok(())
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatsAggregated {
    pub period: String,
    pub total_input: i64,
    pub total_output: i64,
    pub request_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountTokenStats {
    pub account_email: String,
    pub total_input: i64,
    pub total_output: i64,
    pub request_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTokenStats {
    pub model: String,
    pub total_input: i64,
    pub total_output: i64,
    pub request_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatsSummary {
    pub total_requests: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub unique_accounts: i64,
    pub unique_models: i64,
}

/// Get hourly stats for the last N hours.
pub fn get_stats_hourly(hours: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    with_db(|conn| {
        let cutoff = chrono::Utc::now().timestamp() - hours * 3600;
        let mut stmt = conn
            .prepare(
                "SELECT strftime('%Y-%m-%d %H:00', datetime(timestamp, 'unixepoch')) as period,
                        SUM(input_tokens), SUM(output_tokens), COUNT(*)
                 FROM token_usage
                 WHERE timestamp >= ?1
                 GROUP BY period
                 ORDER BY period DESC",
            )
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .query_map(params![cutoff], |row| {
                Ok(TokenStatsAggregated {
                    period: row.get(0)?,
                    total_input: row.get(1)?,
                    total_output: row.get(2)?,
                    request_count: row.get(3)?,
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

/// Get daily stats for the last N days.
pub fn get_stats_daily(days: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    with_db(|conn| {
        let cutoff = chrono::Utc::now().timestamp() - days * 86400;
        let mut stmt = conn
            .prepare(
                "SELECT strftime('%Y-%m-%d', datetime(timestamp, 'unixepoch')) as period,
                        SUM(input_tokens), SUM(output_tokens), COUNT(*)
                 FROM token_usage
                 WHERE timestamp >= ?1
                 GROUP BY period
                 ORDER BY period DESC",
            )
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .query_map(params![cutoff], |row| {
                Ok(TokenStatsAggregated {
                    period: row.get(0)?,
                    total_input: row.get(1)?,
                    total_output: row.get(2)?,
                    request_count: row.get(3)?,
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

/// Get stats grouped by account.
pub fn get_stats_by_account() -> Result<Vec<AccountTokenStats>, String> {
    with_db(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT account_email, SUM(input_tokens), SUM(output_tokens), COUNT(*)
                 FROM token_usage
                 GROUP BY account_email
                 ORDER BY COUNT(*) DESC",
            )
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .query_map([], |row| {
                Ok(AccountTokenStats {
                    account_email: row.get(0)?,
                    total_input: row.get(1)?,
                    total_output: row.get(2)?,
                    request_count: row.get(3)?,
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

/// Get stats grouped by model.
pub fn get_stats_by_model() -> Result<Vec<ModelTokenStats>, String> {
    with_db(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT model, SUM(input_tokens), SUM(output_tokens), COUNT(*)
                 FROM token_usage
                 GROUP BY model
                 ORDER BY COUNT(*) DESC",
            )
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .query_map([], |row| {
                Ok(ModelTokenStats {
                    model: row.get(0)?,
                    total_input: row.get(1)?,
                    total_output: row.get(2)?,
                    request_count: row.get(3)?,
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

/// Get overall summary stats.
pub fn get_stats_summary() -> Result<TokenStatsSummary, String> {
    with_db(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                        COUNT(DISTINCT account_email), COUNT(DISTINCT model)
                 FROM token_usage",
            )
            .map_err(|e| e.to_string())?;

        stmt.query_row([], |row| {
            Ok(TokenStatsSummary {
                total_requests: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                unique_accounts: row.get(3)?,
                unique_models: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();

    fn init_test_db() {
        INIT.call_once(|| {
            // Use in-memory DB for tests
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS token_usage (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                    account_email TEXT NOT NULL,
                    model TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS idx_token_usage_timestamp ON token_usage(timestamp DESC);",
            )
            .unwrap();

            let mut guard = DB.lock().unwrap();
            *guard = Some(conn);
        });
    }

    #[test]
    fn token_stats_record_and_query() {
        init_test_db();

        let before = get_stats_summary().unwrap();
        let base_count = before.total_requests;

        record_usage("user@example.com", "gpt-4", 100, 50).unwrap();
        record_usage("user@example.com", "gpt-4", 200, 100).unwrap();
        record_usage("other@example.com", "claude-3", 300, 150).unwrap();

        let after = get_stats_summary().unwrap();
        assert_eq!(after.total_requests - base_count, 3);
        assert!(after.unique_accounts >= 2);
        assert!(after.unique_models >= 2);
    }

    #[test]
    fn token_stats_by_account() {
        init_test_db();

        // Insert data to ensure non-empty result
        record_usage("test_account@example.com", "gpt-3.5", 10, 5).unwrap();

        let by_account = get_stats_by_account().unwrap();
        assert!(!by_account.is_empty());
    }
}
