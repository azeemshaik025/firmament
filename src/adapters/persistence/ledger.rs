//! Append-only double-entry ledger with `SQLite` persistence.

use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::adapters::persistence::db::{Db, sqlite_error};
use crate::domain::events::{GatewayEvent, RuntimeEvent, SettlementEvent, SwapEvent};
use crate::domain::types::{AmountRaw, AssetId};
use crate::error::AppError;

/// Ledger account buckets used by the Solana-only runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerAccountType {
    /// Working maker custody wallet.
    WorkingCustody,
    /// Funds locked in live HTLC escrow.
    HtlcEscrow,
    /// Funds submitted to HTLC creation but not yet confirmed.
    PendingEscrow,
    /// Circle Gateway balance.
    Gateway,
    /// Gateway refill/deposit in flight.
    PendingGatewayDeposit,
    /// Jupiter rebalance transit account.
    Rebalance,
    /// Operational fees and costs.
    Fees,
    /// Trading and realized P&L account.
    Trading,
    /// Money outside the solver boundary.
    External,
}

impl LedgerAccountType {
    #[must_use]
    const fn as_str(self) -> &'static str {
        match self {
            Self::WorkingCustody => "working_custody",
            Self::HtlcEscrow => "htlc_escrow",
            Self::PendingEscrow => "pending_escrow",
            Self::Gateway => "gateway",
            Self::PendingGatewayDeposit => "pending_gateway_deposit",
            Self::Rebalance => "rebalance",
            Self::Fees => "fees",
            Self::Trading => "trading",
            Self::External => "external",
        }
    }
}

impl Display for LedgerAccountType {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for LedgerAccountType {
    type Error = AppError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "working_custody" => Ok(Self::WorkingCustody),
            "htlc_escrow" => Ok(Self::HtlcEscrow),
            "pending_escrow" => Ok(Self::PendingEscrow),
            "gateway" => Ok(Self::Gateway),
            "pending_gateway_deposit" => Ok(Self::PendingGatewayDeposit),
            "rebalance" => Ok(Self::Rebalance),
            "fees" => Ok(Self::Fees),
            "trading" => Ok(Self::Trading),
            "external" => Ok(Self::External),
            _ => Err(AppError::persistence(format!(
                "invalid ledger account type: {value}"
            ))),
        }
    }
}

/// Concrete account including asset and optional qualifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LedgerAccountId {
    /// Account bucket.
    pub account_type: LedgerAccountType,
    /// Asset held by the account.
    pub asset: AssetId,
    /// Optional account qualifier such as `solana`, `circle:solana`, or `gas`.
    pub qualifier: Option<String>,
}

impl LedgerAccountId {
    #[must_use]
    fn new(account_type: LedgerAccountType, asset: AssetId, qualifier: Option<String>) -> Self {
        Self {
            account_type,
            asset,
            qualifier,
        }
    }

    /// Working custody account for an asset on Solana.
    #[must_use]
    pub fn working(asset: AssetId) -> Self {
        Self::new(
            LedgerAccountType::WorkingCustody,
            asset,
            Some("solana".to_owned()),
        )
    }

    /// Gateway account for an asset.
    #[must_use]
    pub fn gateway(asset: AssetId) -> Self {
        Self::new(
            LedgerAccountType::Gateway,
            asset,
            Some("circle:solana".to_owned()),
        )
    }

    /// Pending Gateway deposit/refill account.
    #[must_use]
    pub fn pending_gateway_deposit(asset: AssetId) -> Self {
        Self::new(
            LedgerAccountType::PendingGatewayDeposit,
            asset,
            Some("circle:solana".to_owned()),
        )
    }

    /// Pending HTLC escrow account for an asset on Solana.
    #[must_use]
    pub fn pending_escrow(asset: AssetId) -> Self {
        Self::new(
            LedgerAccountType::PendingEscrow,
            asset,
            Some("solana".to_owned()),
        )
    }

    /// Confirmed HTLC escrow account for an asset on Solana.
    #[must_use]
    pub fn htlc_escrow(asset: AssetId) -> Self {
        Self::new(
            LedgerAccountType::HtlcEscrow,
            asset,
            Some("solana".to_owned()),
        )
    }

    /// Rebalance transit account.
    #[must_use]
    pub fn rebalance(asset: AssetId) -> Self {
        Self::new(LedgerAccountType::Rebalance, asset, None)
    }

    /// Trading/P&L account.
    #[must_use]
    pub fn trading(asset: AssetId) -> Self {
        Self::new(LedgerAccountType::Trading, asset, None)
    }

    /// Fee account qualified by fee category.
    #[must_use]
    pub fn fees(asset: AssetId, category: impl Into<String>) -> Self {
        Self::new(LedgerAccountType::Fees, asset, Some(category.into()))
    }

    /// External counterparty account qualified by source.
    #[must_use]
    pub fn external(asset: AssetId, qualifier: impl Into<String>) -> Self {
        Self::new(LedgerAccountType::External, asset, Some(qualifier.into()))
    }
}

/// Immutable ledger transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerTransaction {
    /// Transaction UUID.
    pub id: Uuid,
    /// Stable reference type.
    pub reference_type: String,
    /// Reference UUID.
    pub reference_id: Uuid,
    /// Operator-facing description.
    pub description: Option<String>,
    /// Idempotency key for event replay protection.
    pub idempotency_key: Option<String>,
    /// Immutable entries that must balance per asset.
    pub entries: Vec<LedgerEntry>,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
}

impl LedgerTransaction {
    fn validate(&self) -> Result<(), LedgerError> {
        validate_signed_entries(
            self.entries
                .iter()
                .map(|entry| (&entry.account.asset, entry.amount_raw)),
        )
    }
}

/// Immutable signed ledger entry. Positive is debit, negative is credit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Entry UUID.
    pub id: Uuid,
    /// Parent ledger transaction UUID.
    pub transaction_id: Uuid,
    /// Account debited or credited.
    pub account: LedgerAccountId,
    /// Signed raw token amount. Positive is debit, negative is credit.
    pub amount_raw: i128,
    /// Entry timestamp.
    pub created_at: OffsetDateTime,
}

/// Ledger domain failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LedgerError {
    /// A transaction had no entries.
    #[error("ledger transaction must contain at least one entry")]
    EmptyTransaction,
    /// A zero amount was added.
    #[error("ledger entry amount must not be zero")]
    ZeroAmount,
    /// Entries did not net to zero for an asset.
    #[error("ledger transaction does not balance for {asset}: net raw amount {net_amount_raw}")]
    UnbalancedTransaction {
        /// Imbalanced asset.
        asset: AssetId,
        /// Net signed raw amount.
        net_amount_raw: i128,
    },
}

impl From<LedgerError> for AppError {
    fn from(error: LedgerError) -> Self {
        Self::validation(error.to_string())
    }
}

/// Builder for balanced ledger transactions.
pub struct LedgerTransactionBuilder {
    reference_type: String,
    reference_id: Uuid,
    description: Option<String>,
    idempotency_key: Option<String>,
    entries: Vec<PendingEntry>,
}

struct PendingEntry {
    account: LedgerAccountId,
    amount_raw: i128,
}

impl LedgerTransactionBuilder {
    /// Start a transaction builder.
    #[must_use]
    pub fn new(reference_type: impl Into<String>, reference_id: Uuid) -> Self {
        Self {
            reference_type: reference_type.into(),
            reference_id,
            description: None,
            idempotency_key: None,
            entries: Vec::new(),
        }
    }

    /// Set an operator-facing description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set an idempotency key.
    #[must_use]
    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Add a debit entry.
    #[must_use]
    pub fn debit(mut self, account: LedgerAccountId, amount: AmountRaw) -> Self {
        self.entries.push(PendingEntry {
            account,
            amount_raw: i128::from(amount.as_u64()),
        });
        self
    }

    /// Add a credit entry.
    #[must_use]
    pub fn credit(mut self, account: LedgerAccountId, amount: AmountRaw) -> Self {
        self.entries.push(PendingEntry {
            account,
            amount_raw: -i128::from(amount.as_u64()),
        });
        self
    }

    /// Build and validate using the current UTC timestamp.
    ///
    /// # Errors
    ///
    /// Returns a ledger error when the transaction is empty, has zero entries,
    /// or does not balance per asset.
    pub fn build(self) -> Result<LedgerTransaction, LedgerError> {
        self.build_at(OffsetDateTime::now_utc())
    }

    /// Build and validate using a supplied timestamp.
    ///
    /// # Errors
    ///
    /// Returns a ledger error when the transaction is empty, has zero entries,
    /// or does not balance per asset.
    pub fn build_at(self, created_at: OffsetDateTime) -> Result<LedgerTransaction, LedgerError> {
        validate_signed_entries(
            self.entries
                .iter()
                .map(|entry| (&entry.account.asset, entry.amount_raw)),
        )?;

        let transaction_id = Uuid::now_v7();
        let entries = self
            .entries
            .into_iter()
            .map(|entry| LedgerEntry {
                id: Uuid::now_v7(),
                transaction_id,
                account: entry.account,
                amount_raw: entry.amount_raw,
                created_at,
            })
            .collect();

        Ok(LedgerTransaction {
            id: transaction_id,
            reference_type: self.reference_type,
            reference_id: self.reference_id,
            description: self.description,
            idempotency_key: self.idempotency_key,
            entries,
            created_at,
        })
    }
}

fn validate_signed_entries<'asset>(
    entries: impl IntoIterator<Item = (&'asset AssetId, i128)>,
) -> Result<(), LedgerError> {
    let mut saw_entry = false;
    let mut per_asset = BTreeMap::<AssetId, i128>::new();
    for (asset, amount_raw) in entries {
        saw_entry = true;
        if amount_raw == 0 {
            return Err(LedgerError::ZeroAmount);
        }
        *per_asset.entry(asset.clone()).or_default() += amount_raw;
    }

    if !saw_entry {
        return Err(LedgerError::EmptyTransaction);
    }

    if let Some((asset, net_amount_raw)) = per_asset
        .into_iter()
        .find(|(_, net_amount_raw)| *net_amount_raw != 0)
    {
        return Err(LedgerError::UnbalancedTransaction {
            asset,
            net_amount_raw,
        });
    }

    Ok(())
}

/// Result of persisting a ledger transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerSaveOutcome {
    /// Transaction was newly inserted.
    Inserted { transaction_id: Uuid },
    /// Transaction had already been saved via idempotency key or ID.
    AlreadyExists { transaction_id: Uuid },
}

/// Per-account derived balance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerBalance {
    /// Account.
    pub account: LedgerAccountId,
    /// Signed raw balance.
    pub balance_raw: i128,
}

/// Full ledger integrity report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerIntegrityReport {
    /// True when every check passes.
    pub healthy: bool,
    /// Per-asset global net amounts.
    pub global_balances: Vec<LedgerAssetBalance>,
    /// Per-transaction imbalances.
    pub imbalanced_transactions: Vec<ImbalancedLedgerTransaction>,
}

/// Per-asset global net.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerAssetBalance {
    /// Asset.
    pub asset: AssetId,
    /// Net signed raw amount across all entries.
    pub net_amount_raw: i128,
    /// Whether the net amount is zero.
    pub balanced: bool,
}

/// One transaction-level imbalance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImbalancedLedgerTransaction {
    /// Transaction UUID.
    pub transaction_id: Uuid,
    /// Asset.
    pub asset: AssetId,
    /// Net signed raw amount.
    pub net_amount_raw: i128,
}

/// `SQLite`-backed ledger repository.
pub struct SqliteLedgerRepository<'db> {
    db: &'db Db,
}

impl<'db> SqliteLedgerRepository<'db> {
    /// Create a repository over an initialized database.
    #[must_use]
    pub const fn new(db: &'db Db) -> Self {
        Self { db }
    }

    /// Persist a ledger transaction idempotently.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when `SQLite` rejects the write.
    pub fn save_transaction(
        &self,
        transaction: &LedgerTransaction,
    ) -> Result<LedgerSaveOutcome, AppError> {
        transaction.validate()?;
        self.db.with_connection_mut(|connection| {
            let sqlite_tx = connection.transaction().map_err(sqlite_error)?;

            if let Some(idempotency_key) = transaction.idempotency_key.as_deref() {
                let existing = sqlite_tx
                    .query_row(
                        "SELECT id FROM ledger_transactions WHERE idempotency_key = ?1",
                        params![idempotency_key],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                if let Some(existing_id) = existing {
                    let transaction_id = parse_uuid(&existing_id)?;
                    sqlite_tx.rollback().map_err(sqlite_error)?;
                    return Ok(LedgerSaveOutcome::AlreadyExists { transaction_id });
                }
            }

            let existing = sqlite_tx
                .query_row(
                    "SELECT id FROM ledger_transactions WHERE id = ?1",
                    params![transaction.id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            if let Some(existing_id) = existing {
                let transaction_id = parse_uuid(&existing_id)?;
                sqlite_tx.rollback().map_err(sqlite_error)?;
                return Ok(LedgerSaveOutcome::AlreadyExists { transaction_id });
            }

            sqlite_tx
                .execute(
                    "INSERT INTO ledger_transactions
                        (id, reference_type, reference_id, description, idempotency_key, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        transaction.id.to_string(),
                        transaction.reference_type,
                        transaction.reference_id.to_string(),
                        transaction.description,
                        transaction.idempotency_key,
                        format_timestamp(transaction.created_at)?,
                    ],
                )
                .map_err(sqlite_error)?;

            for entry in &transaction.entries {
                sqlite_tx
                    .execute(
                        "INSERT INTO ledger_entries
                            (id, transaction_id, account_type, asset_id, qualifier, amount_raw, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            entry.id.to_string(),
                            entry.transaction_id.to_string(),
                            entry.account.account_type.to_string(),
                            entry.account.asset.as_str(),
                            entry.account.qualifier.as_deref(),
                            entry.amount_raw.to_string(),
                            format_timestamp(entry.created_at)?,
                        ],
                    )
                    .map_err(sqlite_error)?;
            }

            sqlite_tx.commit().map_err(sqlite_error)?;
            Ok(LedgerSaveOutcome::Inserted {
                transaction_id: transaction.id,
            })
        })
    }

    /// Return a derived account balance.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when `SQLite` read or decode fails.
    pub fn account_balance(&self, account: &LedgerAccountId) -> Result<i128, AppError> {
        self.db.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT amount_raw FROM ledger_entries
                     WHERE account_type = ?1
                       AND asset_id = ?2
                       AND ((qualifier IS NULL AND ?3 IS NULL) OR qualifier = ?3)",
                )
                .map_err(sqlite_error)?;
            let rows = statement
                .query_map(
                    params![
                        account.account_type.to_string(),
                        account.asset.as_str(),
                        account.qualifier.as_deref(),
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(sqlite_error)?;

            let mut balance = 0_i128;
            for row in rows {
                balance += parse_amount_raw(&row.map_err(sqlite_error)?)?;
            }
            Ok(balance)
        })
    }

    /// Return all non-zero derived balances.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when `SQLite` read or decode fails.
    pub fn all_balances(&self) -> Result<Vec<LedgerBalance>, AppError> {
        self.db.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT account_type, asset_id, qualifier, amount_raw FROM ledger_entries")
                .map_err(sqlite_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .map_err(sqlite_error)?;

            let mut balances = BTreeMap::<LedgerAccountId, i128>::new();
            for row in rows {
                let (account_type, asset, qualifier, amount_raw) = row.map_err(sqlite_error)?;
                let account = LedgerAccountId::new(
                    LedgerAccountType::try_from(account_type.as_str())?,
                    AssetId::new(asset),
                    qualifier,
                );
                *balances.entry(account).or_default() += parse_amount_raw(&amount_raw)?;
            }

            Ok(balances
                .into_iter()
                .filter_map(|(account, balance_raw)| {
                    (balance_raw != 0).then_some(LedgerBalance {
                        account,
                        balance_raw,
                    })
                })
                .collect())
        })
    }

    /// Count immutable ledger entries.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when `SQLite` read fails.
    pub fn entry_count(&self) -> Result<u64, AppError> {
        self.db.with_connection(|connection| {
            let count = connection
                .query_row("SELECT COUNT(*) FROM ledger_entries", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(sqlite_error)?;
            u64::try_from(count).map_err(|error| AppError::persistence(error.to_string()))
        })
    }

    /// Check per-transaction and global balance invariants.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when `SQLite` read or decode fails.
    pub fn integrity_report(&self) -> Result<LedgerIntegrityReport, AppError> {
        self.db.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT transaction_id, asset_id, amount_raw FROM ledger_entries")
                .map_err(sqlite_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(sqlite_error)?;

            let mut by_transaction_asset = BTreeMap::<(Uuid, AssetId), i128>::new();
            let mut global = BTreeMap::<AssetId, i128>::new();
            for row in rows {
                let (transaction_id, asset, amount_raw) = row.map_err(sqlite_error)?;
                let transaction_id = parse_uuid(&transaction_id)?;
                let asset = AssetId::new(asset);
                let amount_raw = parse_amount_raw(&amount_raw)?;
                *by_transaction_asset
                    .entry((transaction_id, asset.clone()))
                    .or_default() += amount_raw;
                *global.entry(asset).or_default() += amount_raw;
            }

            let imbalanced_transactions = by_transaction_asset
                .into_iter()
                .filter_map(|((transaction_id, asset), net_amount_raw)| {
                    (net_amount_raw != 0).then_some(ImbalancedLedgerTransaction {
                        transaction_id,
                        asset,
                        net_amount_raw,
                    })
                })
                .collect::<Vec<_>>();
            let global_balances = global
                .into_iter()
                .map(|(asset, net_amount_raw)| LedgerAssetBalance {
                    asset,
                    net_amount_raw,
                    balanced: net_amount_raw == 0,
                })
                .collect::<Vec<_>>();
            let healthy = imbalanced_transactions.is_empty()
                && global_balances.iter().all(|balance| balance.balanced);

            Ok(LedgerIntegrityReport {
                healthy,
                global_balances,
                imbalanced_transactions,
            })
        })
    }

    #[cfg(test)]
    fn insert_unchecked_entry_for_test(
        &self,
        transaction_id: Uuid,
        account: &LedgerAccountId,
        amount_raw: i128,
    ) -> Result<(), AppError> {
        self.db.with_connection_mut(|connection| {
            let sqlite_tx = connection.transaction().map_err(sqlite_error)?;
            sqlite_tx
                .execute(
                    "INSERT INTO ledger_transactions
                        (id, reference_type, reference_id, description, idempotency_key, created_at)
                     VALUES (?1, 'corrupt_fixture', ?1, 'unchecked corrupt test fixture', NULL, ?2)",
                    params![transaction_id.to_string(), format_timestamp(OffsetDateTime::now_utc())?],
                )
                .map_err(sqlite_error)?;
            sqlite_tx
                .execute(
                    "INSERT INTO ledger_entries
                        (id, transaction_id, account_type, asset_id, qualifier, amount_raw, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        Uuid::now_v7().to_string(),
                        transaction_id.to_string(),
                            account.account_type.to_string(),
                            account.asset.as_str(),
                            account.qualifier.as_deref(),
                        amount_raw.to_string(),
                        format_timestamp(OffsetDateTime::now_utc())?,
                    ],
                )
                .map_err(sqlite_error)?;
            sqlite_tx.commit().map_err(sqlite_error)
        })
    }
}

/// Result from consuming one runtime event into ledger transactions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerConsumeSummary {
    /// Number of newly inserted ledger transactions.
    pub inserted_transactions: usize,
    /// Number of idempotent replays skipped.
    pub idempotent_replays: usize,
    /// Reasons no ledger transaction was created for parts of the event.
    pub ignored: Vec<String>,
}

/// Runtime event consumer that writes ledger transactions.
pub struct LedgerEventConsumer<'db> {
    repository: SqliteLedgerRepository<'db>,
}

impl<'db> LedgerEventConsumer<'db> {
    /// Create a ledger event consumer.
    #[must_use]
    pub const fn new(db: &'db Db) -> Self {
        Self {
            repository: SqliteLedgerRepository::new(db),
        }
    }

    /// Consume one runtime event.
    ///
    /// # Errors
    ///
    /// Returns ledger or persistence errors for events that contain enough data
    /// to create a transaction but fail validation or persistence.
    pub fn consume(&self, event: &RuntimeEvent) -> Result<LedgerConsumeSummary, AppError> {
        let mut summary = LedgerConsumeSummary::default();
        let transactions = transactions_for_event(event, &mut summary)?;

        for transaction in transactions {
            match self.repository.save_transaction(&transaction)? {
                LedgerSaveOutcome::Inserted { .. } => summary.inserted_transactions += 1,
                LedgerSaveOutcome::AlreadyExists { .. } => summary.idempotent_replays += 1,
            }
        }

        Ok(summary)
    }
}

#[allow(clippy::too_many_lines)]
fn transactions_for_event(
    event: &RuntimeEvent,
    summary: &mut LedgerConsumeSummary,
) -> Result<Vec<LedgerTransaction>, AppError> {
    let mut transactions = Vec::new();
    match event {
        RuntimeEvent::Gateway(GatewayEvent::RefillRequested { metadata, amount }) => {
            transactions.push(
                LedgerTransactionBuilder::new("gateway_refill_requested", metadata.event_id)
                    .description("Gateway refill requested")
                    .idempotency_key(format!(
                        "ledger:event:{}:gateway_refill_requested",
                        metadata.event_id
                    ))
                    .debit(
                        LedgerAccountId::pending_gateway_deposit(amount.asset.clone()),
                        amount.amount_raw,
                    )
                    .credit(
                        LedgerAccountId::gateway(amount.asset.clone()),
                        amount.amount_raw,
                    )
                    .build_at(metadata.occurred_at)?,
            );
        }
        RuntimeEvent::Gateway(GatewayEvent::RefillCompleted { metadata, receipt }) => {
            transactions.push(
                LedgerTransactionBuilder::new("gateway_refill_completed", metadata.event_id)
                    .description("Gateway refill completed into working custody")
                    .idempotency_key(format!(
                        "ledger:event:{}:gateway_refill_completed",
                        metadata.event_id
                    ))
                    .debit(
                        LedgerAccountId::working(receipt.amount.asset.clone()),
                        receipt.amount.amount_raw,
                    )
                    .credit(
                        LedgerAccountId::pending_gateway_deposit(receipt.amount.asset.clone()),
                        receipt.amount.amount_raw,
                    )
                    .build_at(metadata.occurred_at)?,
            );
        }
        RuntimeEvent::Swap(SwapEvent::Executed { metadata, receipt }) => {
            if let Some(output_amount) = &receipt.output_amount {
                transactions.push(
                    LedgerTransactionBuilder::new("swap_executed", metadata.event_id)
                        .description("Jupiter swap output arrived in working custody")
                        .idempotency_key(format!(
                            "ledger:event:{}:swap_executed_output",
                            metadata.event_id
                        ))
                        .debit(
                            LedgerAccountId::working(output_amount.asset.clone()),
                            output_amount.amount_raw,
                        )
                        .credit(
                            LedgerAccountId::rebalance(output_amount.asset.clone()),
                            output_amount.amount_raw,
                        )
                        .build_at(metadata.occurred_at)?,
                );
            } else {
                summary
                    .ignored
                    .push("swap_executed missing output_amount".to_owned());
            }
        }
        RuntimeEvent::Gateway(GatewayEvent::BalanceChecked { .. }) => {
            summary
                .ignored
                .push("gateway balance check is a snapshot, not a movement".to_owned());
        }
        RuntimeEvent::Gateway(GatewayEvent::Failed { .. }) => {
            summary
                .ignored
                .push("gateway failure has no token movement amount".to_owned());
        }
        RuntimeEvent::Settlement(settlement_event) => {
            let reason = match settlement_event {
                SettlementEvent::Started { .. } => "settlement started has no asset amount",
                SettlementEvent::Initiated { .. } => "settlement initiated receipt has no amount",
                SettlementEvent::Redeemed { .. } => "settlement redeemed receipt has no amount",
                SettlementEvent::Refunded { .. } => "settlement refunded receipt has no amount",
                SettlementEvent::StatusChanged { .. } => "settlement status change has no amount",
                SettlementEvent::Failed { .. } => "settlement failure has no token movement amount",
            };
            summary.ignored.push(reason.to_owned());
        }
        RuntimeEvent::Quote(_) => {
            summary
                .ignored
                .push("quote event has no internal token movement".to_owned());
        }
        RuntimeEvent::Risk(_) => {
            summary
                .ignored
                .push("risk event has no token movement".to_owned());
        }
        RuntimeEvent::Inventory(_) => {
            summary.ignored.push(
                "inventory event is observational; balances stay derived from entries".to_owned(),
            );
        }
        RuntimeEvent::System(_) => {
            summary
                .ignored
                .push("system event has no token movement".to_owned());
        }
        RuntimeEvent::Swap(SwapEvent::PriceObserved { .. } | SwapEvent::Quoted { .. }) => {
            summary
                .ignored
                .push("swap price/quote event has no executed token movement".to_owned());
        }
        RuntimeEvent::Swap(SwapEvent::Failed { .. }) => {
            summary
                .ignored
                .push("swap failure has no token movement amount".to_owned());
        }
    }

    Ok(transactions)
}

fn format_timestamp(timestamp: OffsetDateTime) -> Result<String, AppError> {
    timestamp
        .format(&Rfc3339)
        .map_err(|error| AppError::persistence(error.to_string()))
}

fn parse_uuid(value: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|error| AppError::persistence(error.to_string()))
}

fn parse_amount_raw(value: &str) -> Result<i128, AppError> {
    value
        .parse::<i128>()
        .map_err(|error| AppError::persistence(error.to_string()))
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;
    use crate::domain::events::{EventMetadata, GatewayEvent, RuntimeEvent, SwapEvent};
    use crate::domain::types::{AssetPair, RuntimeRunId, SwapReceipt, TokenAmount, TxSignature};

    fn usdc() -> AssetId {
        AssetId::from("USDC")
    }

    fn sol() -> AssetId {
        AssetId::from("SOL")
    }

    fn fixed_time() -> OffsetDateTime {
        datetime!(2026-04-30 00:00 UTC)
    }

    fn test_db() -> Db {
        Db::open_in_memory().unwrap()
    }

    fn balanced_tx() -> LedgerTransaction {
        LedgerTransactionBuilder::new("ledger_test", Uuid::new_v4())
            .description("seed working USDC")
            .idempotency_key("ledger-test-seed")
            .debit(LedgerAccountId::working(usdc()), AmountRaw::new(1_000_000))
            .credit(
                LedgerAccountId::external(usdc(), "seed"),
                AmountRaw::new(1_000_000),
            )
            .build_at(fixed_time())
            .unwrap()
    }

    #[test]
    fn ledger_balanced_transaction_accepted() {
        let transaction = balanced_tx();

        assert_eq!(transaction.entries.len(), 2);
        assert_eq!(
            transaction
                .entries
                .iter()
                .map(|entry| entry.amount_raw)
                .sum::<i128>(),
            0
        );
    }

    #[test]
    fn ledger_unbalanced_transaction_rejected() {
        let result = LedgerTransactionBuilder::new("ledger_test", Uuid::new_v4())
            .debit(LedgerAccountId::working(usdc()), AmountRaw::new(1))
            .build_at(fixed_time());

        assert!(matches!(
            result,
            Err(LedgerError::UnbalancedTransaction { .. })
        ));
    }

    #[test]
    fn ledger_zero_amount_rejected() {
        let result = LedgerTransactionBuilder::new("ledger_test", Uuid::new_v4())
            .debit(LedgerAccountId::working(usdc()), AmountRaw::new(0))
            .credit(LedgerAccountId::external(usdc(), "seed"), AmountRaw::new(0))
            .build_at(fixed_time());

        assert_eq!(result.unwrap_err(), LedgerError::ZeroAmount);
    }

    #[test]
    fn ledger_idempotency_behavior_does_not_duplicate_entries() {
        let db = test_db();
        let repository = SqliteLedgerRepository::new(&db);
        let transaction = balanced_tx();

        assert!(matches!(
            repository.save_transaction(&transaction).unwrap(),
            LedgerSaveOutcome::Inserted { .. }
        ));
        assert!(matches!(
            repository.save_transaction(&transaction).unwrap(),
            LedgerSaveOutcome::AlreadyExists { .. }
        ));
        assert_eq!(repository.entry_count().unwrap(), 2);
    }

    #[test]
    fn ledger_derived_balances_are_summed_from_entries() {
        let db = test_db();
        let repository = SqliteLedgerRepository::new(&db);
        repository.save_transaction(&balanced_tx()).unwrap();

        assert_eq!(
            repository
                .account_balance(&LedgerAccountId::working(usdc()))
                .unwrap(),
            1_000_000
        );
        assert_eq!(
            repository
                .account_balance(&LedgerAccountId::external(usdc(), "seed"))
                .unwrap(),
            -1_000_000
        );
    }

    #[test]
    fn ledger_integrity_report_flags_imbalanced_fixture() {
        let db = test_db();
        let repository = SqliteLedgerRepository::new(&db);
        repository.save_transaction(&balanced_tx()).unwrap();

        let healthy = repository.integrity_report().unwrap();
        assert!(healthy.healthy);

        let corrupt_tx_id = Uuid::new_v4();
        let corrupt_account = LedgerAccountId::working(usdc());
        repository
            .insert_unchecked_entry_for_test(corrupt_tx_id, &corrupt_account, 42)
            .unwrap();

        let report = repository.integrity_report().unwrap();
        assert!(!report.healthy);
        assert_eq!(report.imbalanced_transactions.len(), 1);
        assert_eq!(
            report.imbalanced_transactions[0].transaction_id,
            corrupt_tx_id
        );
    }

    #[test]
    fn ledger_gateway_events_map_to_pending_and_working_accounts() {
        let db = test_db();
        let consumer = LedgerEventConsumer::new(&db);
        let repository = SqliteLedgerRepository::new(&db);
        let metadata = EventMetadata::new(RuntimeRunId::generate());

        let requested = RuntimeEvent::Gateway(GatewayEvent::RefillRequested {
            metadata,
            amount: TokenAmount::new(usdc(), AmountRaw::new(2_000_000)),
        });
        let completed = RuntimeEvent::Gateway(GatewayEvent::RefillCompleted {
            metadata: EventMetadata::new(metadata.run_id),
            receipt: crate::domain::types::GatewayReceipt {
                amount: TokenAmount::new(usdc(), AmountRaw::new(2_000_000)),
                provider_transfer_id: Some("circle-transfer".to_owned()),
                signature: None,
            },
        });

        assert_eq!(
            consumer.consume(&requested).unwrap().inserted_transactions,
            1
        );
        assert_eq!(
            consumer.consume(&completed).unwrap().inserted_transactions,
            1
        );

        assert_eq!(
            repository
                .account_balance(&LedgerAccountId::pending_gateway_deposit(usdc()))
                .unwrap(),
            0
        );
        assert_eq!(
            repository
                .account_balance(&LedgerAccountId::working(usdc()))
                .unwrap(),
            2_000_000
        );
        assert_eq!(
            repository
                .account_balance(&LedgerAccountId::gateway(usdc()))
                .unwrap(),
            -2_000_000
        );
    }

    #[test]
    fn ledger_swap_executed_event_records_output_arrival() {
        let db = test_db();
        let consumer = LedgerEventConsumer::new(&db);
        let repository = SqliteLedgerRepository::new(&db);

        let event = RuntimeEvent::Swap(SwapEvent::Executed {
            metadata: EventMetadata::new(RuntimeRunId::generate()),
            receipt: SwapReceipt {
                trade_id: None,
                signature: TxSignature::new("swap-signature"),
                output_amount: Some(TokenAmount::new(sol(), AmountRaw::new(500_000_000))),
            },
        });

        assert_eq!(consumer.consume(&event).unwrap().inserted_transactions, 1);
        assert_eq!(
            repository
                .account_balance(&LedgerAccountId::working(sol()))
                .unwrap(),
            500_000_000
        );
        assert_eq!(
            repository
                .account_balance(&LedgerAccountId::rebalance(sol()))
                .unwrap(),
            -500_000_000
        );
    }

    #[test]
    fn ledger_quote_and_inventory_events_without_amounts_are_ignored() {
        let db = test_db();
        let consumer = LedgerEventConsumer::new(&db);
        let event = RuntimeEvent::Swap(SwapEvent::Failed {
            metadata: EventMetadata::new(RuntimeRunId::generate()),
            pair: Some(AssetPair::new(usdc(), sol())),
            reason: "route unavailable".to_owned(),
        });

        let summary = consumer.consume(&event).unwrap();
        assert_eq!(summary.inserted_transactions, 0);
        assert!(!summary.ignored.is_empty());
    }
}
