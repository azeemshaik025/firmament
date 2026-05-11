//! `SQLite` database bootstrap for local durable runtime state.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::AppError;

/// Thin `SQLite` owner used by repositories in this crate.
pub struct Db {
    connection: Mutex<Connection>,
}

const BASE_SCHEMA_SQL: &str = r"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS ledger_transactions (
    id TEXT PRIMARY KEY NOT NULL,
    reference_type TEXT NOT NULL,
    reference_id TEXT NOT NULL,
    description TEXT,
    idempotency_key TEXT UNIQUE,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS ledger_entries (
    id TEXT PRIMARY KEY NOT NULL,
    transaction_id TEXT NOT NULL REFERENCES ledger_transactions(id),
    account_type TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    qualifier TEXT,
    amount_raw TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_ledger_entries_account
    ON ledger_entries(account_type, asset_id, qualifier);

CREATE INDEX IF NOT EXISTS idx_ledger_entries_transaction_asset
    ON ledger_entries(transaction_id, asset_id);

CREATE TABLE IF NOT EXISTS pnl_estimates (
    id TEXT PRIMARY KEY NOT NULL,
    source_event_id TEXT NOT NULL,
    reference_type TEXT NOT NULL,
    reference_id TEXT NOT NULL,
    category TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    amount_raw TEXT NOT NULL,
    usdc_value TEXT NOT NULL,
    description TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(source_event_id, category, asset_id)
);

CREATE INDEX IF NOT EXISTS idx_pnl_estimates_category
    ON pnl_estimates(category);

CREATE TABLE IF NOT EXISTS runtime_trades (
    trade_id TEXT PRIMARY KEY NOT NULL,
    quote_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    taker_wallet TEXT,
    settlement_status TEXT NOT NULL,
    input_asset TEXT NOT NULL,
    input_amount_raw TEXT NOT NULL,
    output_asset TEXT NOT NULL,
    output_amount_raw TEXT NOT NULL,
    execution_path_json TEXT NOT NULL,
    trade_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_runtime_trades_created_at
    ON runtime_trades(created_at DESC);

CREATE INDEX IF NOT EXISTS idx_runtime_trades_status
    ON runtime_trades(settlement_status);

CREATE INDEX IF NOT EXISTS idx_runtime_trades_taker_wallet_created_at
    ON runtime_trades(taker_wallet, created_at DESC);

CREATE TABLE IF NOT EXISTS runtime_trade_signatures (
    id TEXT PRIMARY KEY NOT NULL,
    trade_id TEXT NOT NULL REFERENCES runtime_trades(trade_id) ON DELETE CASCADE,
    signature_kind TEXT NOT NULL,
    signature TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(trade_id, signature_kind, signature)
);

CREATE INDEX IF NOT EXISTS idx_runtime_trade_signatures_trade
    ON runtime_trade_signatures(trade_id);

CREATE TABLE IF NOT EXISTS runtime_wallet_settlements (
    trade_id TEXT PRIMARY KEY NOT NULL,
    quote_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    taker_wallet TEXT NOT NULL,
    settlement_phase TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    input_asset TEXT NOT NULL,
    input_amount_raw TEXT NOT NULL,
    output_asset TEXT NOT NULL,
    output_amount_raw TEXT NOT NULL,
    state_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_runtime_wallet_settlements_created_at
    ON runtime_wallet_settlements(created_at DESC);
";

impl Db {
    /// Open an in-memory `SQLite` database and initialize the schema.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when `SQLite` cannot open or initialize.
    pub fn open_in_memory() -> Result<Self, AppError> {
        Self::from_connection(Connection::open_in_memory())
    }

    /// Open a `SQLite` database at `path` and initialize the schema.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when `SQLite` cannot open or initialize.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AppError> {
        Self::from_connection(Connection::open(path))
    }

    fn from_connection(connection: rusqlite::Result<Connection>) -> Result<Self, AppError> {
        let db = Self {
            connection: Mutex::new(connection.map_err(sqlite_error)?),
        };
        db.init_schema()?;
        Ok(db)
    }

    /// Initialize all tables required by the ledger and P&L projections.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when schema creation fails.
    pub fn init_schema(&self) -> Result<(), AppError> {
        self.with_connection(|connection| {
            connection
                .execute_batch(BASE_SCHEMA_SQL)
                .map_err(sqlite_error)?;
            ensure_nullable_column(connection, "runtime_trades", "taker_wallet", "TEXT")?;
            connection
                .execute(
                    "CREATE INDEX IF NOT EXISTS idx_runtime_trades_taker_wallet_created_at
                        ON runtime_trades(taker_wallet, created_at DESC)",
                    [],
                )
                .map_err(sqlite_error)?;
            Ok(())
        })
    }

    pub(crate) fn with_connection<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let guard = self
            .connection
            .lock()
            .map_err(|_| AppError::persistence("SQLite connection mutex poisoned"))?;
        f(&guard)
    }

    pub(crate) fn with_connection_mut<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let mut guard = self
            .connection
            .lock()
            .map_err(|_| AppError::persistence("SQLite connection mutex poisoned"))?;
        f(&mut guard)
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sqlite_error(error: rusqlite::Error) -> AppError {
    AppError::persistence(error.to_string())
}

fn ensure_nullable_column(
    connection: &Connection,
    table: &str,
    column: &str,
    column_type: &str,
) -> Result<(), AppError> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut statement = connection.prepare(&pragma).map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_error)?;
    for row in rows {
        if row.map_err(sqlite_error)? == column {
            return Ok(());
        }
    }

    let alter = format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}");
    connection.execute(&alter, []).map_err(sqlite_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Db;

    #[test]
    fn db_schema_initializes_ledger_and_pnl_tables() {
        let db = Db::open_in_memory().unwrap();

        let tables = db
            .with_connection(|connection| {
                let mut statement = connection
                    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                    .map_err(|error| crate::error::AppError::persistence(error.to_string()))?;
                let rows = statement
                    .query_map([], |row| row.get::<_, String>(0))
                    .map_err(|error| crate::error::AppError::persistence(error.to_string()))?;
                let mut names = Vec::new();
                for row in rows {
                    names.push(
                        row.map_err(|error| {
                            crate::error::AppError::persistence(error.to_string())
                        })?,
                    );
                }
                Ok(names)
            })
            .unwrap();

        assert!(tables.contains(&"ledger_transactions".to_owned()));
        assert!(tables.contains(&"ledger_entries".to_owned()));
        assert!(tables.contains(&"pnl_estimates".to_owned()));
        assert!(tables.contains(&"runtime_trades".to_owned()));
        assert!(tables.contains(&"runtime_trade_signatures".to_owned()));
        assert!(tables.contains(&"runtime_wallet_settlements".to_owned()));
    }
}
