//! `SQLite` database bootstrap for local durable runtime state.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::AppError;

/// Thin `SQLite` owner used by repositories in this crate.
pub struct Db {
    connection: Mutex<Connection>,
}

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
                .execute_batch(
                    r"
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
                    ",
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
    }
}
