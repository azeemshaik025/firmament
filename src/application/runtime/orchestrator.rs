//! Event-driven runtime orchestration.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use rusqlite::{OptionalExtension, params};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::adapters::persistence::db::Db;
use crate::adapters::persistence::ledger::{
    LedgerAccountId, LedgerAccountType, LedgerBalance, LedgerEventConsumer, LedgerSaveOutcome,
    LedgerTransaction, SqliteLedgerRepository,
};
use crate::adapters::persistence::pnl::{PnlCategory, PnlEstimate, PnlPriceBook, PnlRepository};
use crate::adapters::solana::htlc::{generate_secret, hash_secret};
use crate::application::rebalance::{
    ActionKey, AutomationActionKind, AutomationLimits, AutomationState, DecisionBlockReason,
    ExcessDepositPlan, GatewayRefillDecision, GatewayRefillPlan, PlannedSwap, RebalanceDecision,
    RebalancePolicy, decide_excess_deposit, decide_gateway_refill, decide_inventory_rebalance,
    decide_native_sol_top_up, submit_gateway_refill, submit_planned_swap,
};
use crate::application::rfq::{
    FirmQuote, QuoteAcceptance, RfqContext, RfqRequest, RfqResponse, accept_quote_for_settlement,
    request_quote,
};
use crate::config::AppConfig;
use crate::config::AssetConfig;
use crate::domain::assets::AssetRegistry;
use crate::domain::events::{
    EventMetadata, GatewayEvent, InventoryEvent, QuoteEvent, RuntimeEvent, SettlementEvent,
    SwapEvent, SystemEvent,
};
use crate::domain::inventory::{
    InventoryPolicy, InventorySnapshot as ValuedInventorySnapshot, raw_to_units,
};
use crate::domain::quote_engine::InventorySnapshot as QuoteInventorySnapshot;
use crate::domain::settlement::{
    SettlementLeg, SettlementPhase, SettlementTerms, TwoSidedSettlement,
};
use crate::domain::types::{
    AmountRaw, AssetId, BalanceSnapshot, ExecutionPath, ExternalHtlcInitiation,
    GatewayRefillRequest, QuoteId, RuntimeRunId, SettlementStatus, TokenAmount, TradeId,
    TxSignature, UnsignedWalletTransaction, WalletAddress, WalletRole,
};
use crate::error::{AppError, AppResult};
use crate::ports::{BalanceReader, GatewayClient, HtlcClient, PriceProvider, SwapExecutor};

use super::bootstrap::AppState;
use super::projection::{PnlProjection, RuntimeHandle};

/// Runtime adapter set used by orchestration. All live protocol effects stay
/// behind these trait boundaries so tests can use fakes and production can wire
/// the real Solana/Jupiter/Gateway clients explicitly.
#[derive(Clone)]
pub struct RuntimeAdapters {
    /// Reference price source used by RFQ quote construction.
    pub price_provider: Arc<dyn PriceProvider>,
    /// Solana HTLC client used for two-sided settlement.
    pub htlc_client: Arc<dyn HtlcClient>,
    /// Jupiter-backed swap executor used by rebalance decisions.
    pub swap_executor: Arc<dyn SwapExecutor>,
    /// Circle Gateway client used for USDC refill decisions.
    pub gateway_client: Arc<dyn GatewayClient>,
    /// Working wallet balance reader used before quote and after settlement.
    pub balance_reader: Arc<dyn BalanceReader>,
}

/// Test and startup knobs for the orchestration layer.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeOrchestratorOptions {
    /// Cumulative automated spend already consumed in this runtime run.
    pub initial_automation_spend_usd: Decimal,
    /// Taker wallet address used by legacy scripted demo RFQs.
    pub demo_taker_wallet: Option<crate::domain::types::WalletAddress>,
    /// Whether this process owns the local taker signer needed for the
    /// current two-wallet local-signer settlement flow.
    pub allow_local_taker_settlement: bool,
}

impl Default for RuntimeOrchestratorOptions {
    fn default() -> Self {
        Self {
            initial_automation_spend_usd: Decimal::ZERO,
            demo_taker_wallet: None,
            allow_local_taker_settlement: true,
        }
    }
}

/// Durable ledger/P&L summary projected from runtime event persistence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeLedgerSummary {
    /// Whether persisted double-entry movements are balanced.
    pub balanced: bool,
    /// Number of persisted ledger entries.
    pub entry_count: usize,
    /// Net USDC-estimated P&L.
    pub net_usdc_estimate: Decimal,
}

/// Snapshot of derived ledger balances and aggregate health used by the
/// public `/v1/runtime/ledger` endpoint.
///
/// `healthy` combines the integrity report check with a per-protected-account
/// non-negative invariant. `entry_count` and `healthy` describe the whole
/// ledger; callers may filter `balances` by `account_type` without affecting
/// the global counters.
#[derive(Debug, Clone)]
pub struct LedgerReadSnapshot {
    /// True when integrity is healthy AND no protected account is negative.
    pub healthy: bool,
    /// Total ledger entry rows.
    pub entry_count: u64,
    /// Non-zero derived balances keyed by `(account_type, asset, qualifier)`.
    pub balances: Vec<LedgerBalance>,
}

impl Default for RuntimeLedgerSummary {
    fn default() -> Self {
        Self {
            balanced: true,
            entry_count: 0,
            net_usdc_estimate: Decimal::ZERO,
        }
    }
}

/// One trade-bound transaction signature with kind discriminator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeSignature {
    /// Stable kind discriminator for downstream UI rendering.
    pub kind: TradeSignatureKind,
    /// Solana signature string.
    pub signature: String,
}

/// Stable discriminator for trade-bound signatures. Background rebalance
/// signatures are never reported here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeSignatureKind {
    /// Taker leg HTLC submitted/confirmed.
    TakerLock,
    /// Taker leg HTLC redeemed by the maker.
    TakerRedeem,
    /// Taker leg HTLC refunded.
    TakerRefund,
    /// Maker leg HTLC submitted/confirmed.
    MakerLock,
    /// Maker leg HTLC redeemed by the taker.
    MakerRedeem,
    /// Maker leg HTLC refunded.
    MakerRefund,
    /// Gateway burn intent.
    GatewayBurn,
    /// Gateway mint receipt.
    GatewayMint,
    /// Jupiter swap submitted as part of a Gateway-to-DEX trade leg.
    JupiterSwap,
}

impl TradeSignatureKind {
    /// Map an HTLC receipt's `(leg, status)` pair into the trade-summary
    /// signature kind. Returns `None` for transitional or pending states
    /// where no trade-summary entry should be appended.
    ///
    /// Convention: `leg` identifies WHICH HTLC the receipt describes (not
    /// who acted). So a `Redeemed` receipt with `leg = MakerOutput` means
    /// "maker's HTLC was redeemed", which by lifecycle convention happens
    /// by the taker → kind = `taker_redeem`.
    #[must_use]
    pub const fn from_htlc_receipt(
        leg: crate::domain::settlement::SettlementLeg,
        status: SettlementStatus,
    ) -> Option<Self> {
        use crate::domain::settlement::SettlementLeg;
        match (leg, status) {
            (
                SettlementLeg::TakerInput,
                SettlementStatus::Initiated | SettlementStatus::Pending,
            ) => Some(Self::TakerLock),
            (
                SettlementLeg::MakerOutput,
                SettlementStatus::Initiated | SettlementStatus::Pending,
            ) => Some(Self::MakerLock),
            (SettlementLeg::MakerOutput, SettlementStatus::Redeemed) => Some(Self::TakerRedeem),
            (SettlementLeg::TakerInput, SettlementStatus::Redeemed) => Some(Self::MakerRedeem),
            (SettlementLeg::TakerInput, SettlementStatus::Refunded) => Some(Self::TakerRefund),
            (SettlementLeg::MakerOutput, SettlementStatus::Refunded) => Some(Self::MakerRefund),
            _ => None,
        }
    }
}

/// Operator-facing trade record held by the runtime projection layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeTrade {
    /// Quote that created this trade.
    pub quote_id: QuoteId,
    /// Runtime trade identifier.
    pub trade_id: TradeId,
    /// Runtime invocation that originally created this trade.
    pub run_id: RuntimeRunId,
    /// Browser wallet that requested the quote when attribution is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taker_wallet: Option<WalletAddress>,
    /// HTLC expiry for wallet settlements when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<OffsetDateTime>,
    /// Latest settlement status.
    pub settlement_status: SettlementStatus,
    /// Solana signatures observed during settlement.
    pub tx_signatures: Vec<TxSignature>,
    /// Trade-bound transaction signatures with `kind` discriminator. Used
    /// by `/v1/runtime/trades`. Background rebalance signatures are
    /// excluded.
    pub tx_signature_kinds: Vec<TradeSignature>,
    /// UTC creation timestamp; used to sort trades newest-first in the
    /// `/v1/runtime/trades` response.
    pub created_at: OffsetDateTime,
    /// Taker input amount.
    pub input_amount: TokenAmount,
    /// Maker output amount.
    pub output_amount: TokenAmount,
    /// Resolved execution path. In-memory only — not persisted across restart.
    pub execution_path: ExecutionPath,
}

/// Aggregate counts for persisted and active runtime trades.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeTradeCounts {
    /// Every terminal or active trade known to the runtime.
    pub total_count: usize,
    /// Terminal trades whose status is redeemed.
    pub successful_count: usize,
    /// Active wallet settlements not yet terminal.
    pub active_count: usize,
    /// Terminal trades whose status is refunded.
    pub refunded_count: usize,
    /// Terminal trades whose status is failed.
    pub failed_count: usize,
}

/// Response returned when a browser-wallet settlement is started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletSettlementStart {
    /// Quote accepted for wallet settlement.
    pub quote_id: QuoteId,
    /// Trade created for settlement tracking.
    pub trade_id: TradeId,
    /// Unsigned taker lock transaction for the connected wallet to sign.
    pub taker_lock_transaction: UnsignedWalletTransaction,
    /// HTLC expiry inherited from the accepted quote.
    pub expires_at: OffsetDateTime,
}

/// Response returned after the browser taker lock signature is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletTakerLockResult {
    /// Trade being settled.
    pub trade_id: TradeId,
    /// Current settlement status.
    pub settlement_status: SettlementStatus,
    /// Maker lock signature submitted by the backend.
    pub maker_lock_signature: Option<TxSignature>,
    /// All observed signatures so far.
    pub tx_signatures: Vec<TxSignature>,
}

/// Response returned when the browser asks for a redeem transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletTakerRedeemPreparation {
    /// Trade being settled.
    pub trade_id: TradeId,
    /// Unsigned taker redeem transaction for the connected wallet to sign.
    pub taker_redeem_transaction: UnsignedWalletTransaction,
}

/// Response returned when the browser taker redeem signature is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletTakerRedeemResult {
    /// Final runtime trade record.
    pub trade: RuntimeTrade,
    /// Maker redeem signature submitted by the backend.
    pub maker_redeem_signature: Option<TxSignature>,
}

/// Response returned when a browser-wallet settlement is resumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletSettlementResume {
    /// Trade being resumed.
    pub trade_id: TradeId,
    /// Quote that created this settlement.
    pub quote_id: QuoteId,
    /// Runtime run that originally created this settlement.
    pub run_id: RuntimeRunId,
    /// Current pure settlement phase.
    pub settlement_phase: SettlementPhase,
    /// Coarse settlement status used by existing API/UI surfaces.
    pub settlement_status: SettlementStatus,
    /// Fresh taker lock transaction when the taker has not locked funds yet.
    pub taker_lock_transaction: Option<UnsignedWalletTransaction>,
    /// Fresh taker redeem transaction when a preimage was supplied separately.
    pub taker_redeem_transaction: Option<UnsignedWalletTransaction>,
    /// Fresh taker refund transaction once the taker lock is refundable.
    pub taker_refund_transaction: Option<UnsignedWalletTransaction>,
    /// Known raw signatures.
    pub tx_signatures: Vec<TxSignature>,
    /// Known typed signatures.
    pub tx_signature_kinds: Vec<TradeSignature>,
    /// Settlement expiry inherited from the quote.
    pub expires_at: OffsetDateTime,
}

/// Response returned when a browser asks for a refund transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletTakerRefundPreparation {
    /// Trade being refunded.
    pub trade_id: TradeId,
    /// Unsigned taker refund transaction for the connected wallet to sign.
    pub taker_refund_transaction: UnsignedWalletTransaction,
}

/// Response returned when a browser taker refund signature is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletTakerRefundResult {
    /// Final runtime trade record.
    pub trade: RuntimeTrade,
    /// Maker refund signature submitted by the backend, when a maker leg was live.
    pub maker_refund_signature: Option<TxSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WalletSettlementState {
    quote: FirmQuote,
    settlement: TwoSidedSettlement,
    taker_lock_request: ExternalHtlcInitiation,
    taker_wallet: WalletAddress,
    tx_signatures: Vec<TxSignature>,
    tx_signature_kinds: Vec<TradeSignature>,
    run_id: RuntimeRunId,
    created_at: OffsetDateTime,
}

/// Summary of one post-settlement automation pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutomationRunSummary {
    /// Whether an inventory snapshot was refreshed and published.
    pub inventory_refreshed: bool,
    /// Number of swap actions completed.
    pub completed_swaps: usize,
    /// Number of Gateway refills completed.
    pub completed_gateway_refills: usize,
    /// Number of Gateway excess deposits completed.
    pub completed_gateway_deposits: usize,
    /// Stable block reasons emitted by decision functions.
    pub blocked_reasons: Vec<DecisionBlockReason>,
}

/// Durable event persistence for ledger and P&L projections.
pub struct RuntimePersistence {
    db: Arc<Db>,
    registry: AssetRegistry,
    price_book: Mutex<PnlPriceBook>,
}

impl RuntimePersistence {
    /// Open persistence at a configured `SQLite` path.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when the database cannot be opened or
    /// initialized.
    pub fn open(path: impl AsRef<std::path::Path>, registry: AssetRegistry) -> AppResult<Self> {
        Ok(Self {
            db: Arc::new(Db::open(path)?),
            registry,
            price_book: Mutex::new(PnlPriceBook::default()),
        })
    }

    /// Open in-memory persistence for tests.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when the in-memory database cannot initialize.
    pub fn open_in_memory(registry: AssetRegistry) -> AppResult<Self> {
        Ok(Self {
            db: Arc::new(Db::open_in_memory()?),
            registry,
            price_book: Mutex::new(PnlPriceBook::default()),
        })
    }

    /// Consume a runtime event into ledger and P&L persistence.
    ///
    /// # Errors
    ///
    /// Returns persistence or valuation errors from durable consumers.
    pub fn consume(&self, event: &RuntimeEvent) -> AppResult<()> {
        LedgerEventConsumer::new(self.db.as_ref()).consume(event)?;
        self.consume_pnl(event)?;
        Ok(())
    }

    /// Return a combined ledger/P&L summary.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when summaries cannot be read.
    pub fn summary(&self) -> AppResult<RuntimeLedgerSummary> {
        let ledger = SqliteLedgerRepository::new(self.db.as_ref());
        let integrity = ledger.integrity_report()?;
        let entry_count = usize::try_from(ledger.entry_count()?)
            .map_err(|error| AppError::persistence(error.to_string()))?;
        let pnl = PnlRepository::new(self.db.as_ref()).summary()?;

        Ok(RuntimeLedgerSummary {
            balanced: integrity.healthy,
            entry_count,
            net_usdc_estimate: pnl.net_usdc,
        })
    }

    /// Return the ledger working-custody balance for an asset on Solana.
    ///
    /// Negative balances are clipped to zero — the gate only cares about
    /// quotable supply, not signed integrity. Use [`Self::summary`] to
    /// surface integrity drift instead.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when the ledger read fails.
    pub fn working_custody_balance(&self, asset: &AssetId) -> AppResult<AmountRaw> {
        let ledger = SqliteLedgerRepository::new(self.db.as_ref());
        let balance = ledger.account_balance(&LedgerAccountId::working(asset.clone()))?;
        let clamped = u64::try_from(balance.max(0)).unwrap_or(u64::MAX);
        Ok(AmountRaw::new(clamped))
    }

    /// Return the FREE Gateway balance for an asset, defined as the Gateway
    /// account balance minus the sum of every `gateway_reserved:asset:*`
    /// qualifier.
    ///
    /// Used by the RFQ Gateway-quoteability gate: when `working_custody` is
    /// insufficient for the requested output, the maker may still source the
    /// fill via the Gateway-backed path provided the free Gateway balance
    /// covers it after deducting in-flight reservations.
    ///
    /// Negative balances (gateway drift) are clamped to zero. Use
    /// [`Self::summary`] to surface integrity drift instead.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when either ledger read fails.
    pub fn gateway_free_balance(&self, asset: &AssetId) -> AppResult<AmountRaw> {
        let ledger = SqliteLedgerRepository::new(self.db.as_ref());
        let gateway = ledger.account_balance(&LedgerAccountId::gateway(asset.clone()))?;
        let reserved =
            ledger.aggregate_balance_by_type(LedgerAccountType::GatewayReserved, asset)?;
        // Clamp reserved to non-negative: a negative reserved balance would
        // indicate ledger drift and must not bump the gate.
        let free = gateway.saturating_sub(reserved.max(0));
        let clamped = u64::try_from(free.max(0)).unwrap_or(u64::MAX);
        Ok(AmountRaw::new(clamped))
    }

    /// Persist a ledger transaction directly. Used by tests and operator
    /// reconciliation flows that need to seed or adjust ledger state without
    /// going through the runtime event consumer.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when the transaction is invalid or the
    /// `SQLite` write fails.
    pub fn save_ledger_transaction(
        &self,
        transaction: &LedgerTransaction,
    ) -> AppResult<LedgerSaveOutcome> {
        SqliteLedgerRepository::new(self.db.as_ref()).save_transaction(transaction)
    }

    /// Read a single ledger account balance. Used by the reconciliation
    /// worker.
    ///
    /// # Errors
    ///
    /// Returns persistence errors when the read fails.
    pub fn account_balance(&self, account: &LedgerAccountId) -> AppResult<i128> {
        SqliteLedgerRepository::new(self.db.as_ref()).account_balance(account)
    }

    /// Sum signed balances across every qualifier for `(account_type, asset)`.
    /// Used by the reconciliation worker for `reserved`, `gateway_reserved`,
    /// `pending_dex_spend`, `receivable`, etc.
    ///
    /// # Errors
    ///
    /// Returns persistence errors when the read fails.
    pub fn aggregate_balance_by_type(
        &self,
        account_type: LedgerAccountType,
        asset: &AssetId,
    ) -> AppResult<i128> {
        SqliteLedgerRepository::new(self.db.as_ref()).aggregate_balance_by_type(account_type, asset)
    }

    /// Visit reconciliation idempotency keys whose stored value starts with
    /// the supplied prefix. Used by tests to assert restart-time
    /// idempotency.
    ///
    /// # Errors
    ///
    /// Returns persistence errors when the read fails.
    pub fn with_recon_idempotency_keys<R>(
        &self,
        prefix: &str,
        visitor: impl FnOnce(&[String]) -> R,
    ) -> AppResult<R> {
        let pattern = format!("{prefix}%");
        let rows: Vec<String> = self.db.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT idempotency_key FROM ledger_transactions
                     WHERE idempotency_key LIKE ?1",
                )
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            let mapped = statement
                .query_map([&pattern], |row| row.get::<_, String>(0))
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            let mut acc = Vec::new();
            for row in mapped {
                acc.push(row.map_err(crate::adapters::persistence::db::sqlite_error)?);
            }
            Ok::<_, AppError>(acc)
        })?;
        Ok(visitor(&rows))
    }

    /// Reseed the in-memory daily idempotency-sequence counters from
    /// persisted reconciliation transactions for the given UTC date.
    ///
    /// Idempotency keys are formatted `recon:{scope}:{asset}:{utc_date}:{seq}`.
    /// On startup the worker invokes this to recover the highest already-used
    /// sequence per `(scope, asset, date)` so the daily counter does not
    /// reuse a previously persisted key after a restart.
    ///
    /// # Errors
    ///
    /// Returns persistence errors when the read fails.
    pub fn seed_recon_sequences(
        &self,
        utc_date: &str,
        sequences: &mut std::collections::HashMap<(&'static str, AssetId, String), u64>,
    ) -> AppResult<()> {
        let prefix_wallet = "recon:wallet:";
        let prefix_gateway = "recon:gateway:";
        let pattern = format!("recon:%:%:{utc_date}:%");
        let rows: Vec<String> = self.db.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT idempotency_key FROM ledger_transactions
                     WHERE idempotency_key LIKE ?1",
                )
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            let mapped = statement
                .query_map([&pattern], |row| row.get::<_, String>(0))
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            let mut acc = Vec::new();
            for row in mapped {
                acc.push(row.map_err(crate::adapters::persistence::db::sqlite_error)?);
            }
            Ok::<_, AppError>(acc)
        })?;

        for key in rows {
            // Format: recon:{scope}:{asset}:{utc_date}:{seq}
            let scope_label: &'static str = if key.starts_with(prefix_wallet) {
                "wallet"
            } else if key.starts_with(prefix_gateway) {
                "gateway"
            } else {
                continue;
            };
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() != 5 {
                continue;
            }
            // parts: ["recon", scope, asset, utc_date, seq]
            if parts[3] != utc_date {
                continue;
            }
            let asset = AssetId::from(parts[2]);
            let Ok(seq) = parts[4].parse::<u64>() else {
                continue;
            };
            let entry = sequences
                .entry((scope_label, asset, utc_date.to_owned()))
                .or_insert(0);
            if seq > *entry {
                *entry = seq;
            }
        }
        Ok(())
    }

    /// Return the asset registry used by valuation and presentation layers.
    #[must_use]
    pub fn asset_registry(&self) -> &AssetRegistry {
        &self.registry
    }

    /// Return a derived ledger snapshot suitable for the public read
    /// endpoint. Combines `all_balances`, `entry_count`, and the integrity
    /// report into a single atomic-ish read. Health is `true` only when the
    /// integrity report is healthy AND no protected account holds a
    /// negative balance.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when any underlying read fails.
    pub fn ledger_snapshot(&self) -> AppResult<LedgerReadSnapshot> {
        let ledger = SqliteLedgerRepository::new(self.db.as_ref());
        let balances = ledger.all_balances()?;
        let entry_count = ledger.entry_count()?;
        let integrity = ledger.integrity_report()?;

        let protected_types = LedgerAccountType::protected_account_types();
        let protected_negative = balances.iter().any(|balance| {
            balance.balance_raw < 0 && protected_types.contains(&balance.account.account_type)
        });
        let healthy = integrity.healthy && !protected_negative;

        Ok(LedgerReadSnapshot {
            healthy,
            entry_count,
            balances,
        })
    }

    /// Return the latest P&L projection.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when the P&L summary cannot be read.
    pub fn pnl_projection(&self) -> AppResult<PnlProjection> {
        let summary = PnlRepository::new(self.db.as_ref()).summary()?;
        Ok(PnlProjection {
            realized_spread_usdc_estimate: summary.realized_spread_usdc,
            fees_usdc_estimate: summary.fees_usdc,
            rebalance_cost_usdc_estimate: summary.rebalance_cost_usdc,
            net_usdc_estimate: summary.net_usdc,
        })
    }

    /// Persist a terminal trade and its typed signatures.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when SQLite or serialization fails.
    pub fn save_trade(&self, trade: &RuntimeTrade) -> AppResult<()> {
        let trade_json = serde_json::to_string(trade)
            .map_err(|error| AppError::persistence(format!("serialize runtime trade: {error}")))?;
        let execution_path_json = serde_json::to_string(&trade.execution_path)
            .map_err(|error| AppError::persistence(format!("serialize execution path: {error}")))?;
        let created_at = format_runtime_timestamp(trade.created_at)?;
        let updated_at = format_runtime_timestamp(OffsetDateTime::now_utc())?;
        self.db.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            transaction
                .execute(
                    "INSERT INTO runtime_trades (
                        trade_id, quote_id, run_id, taker_wallet, settlement_status,
                        input_asset, input_amount_raw, output_asset, output_amount_raw,
                        execution_path_json, trade_json, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(trade_id) DO UPDATE SET
                        quote_id = excluded.quote_id,
                        run_id = excluded.run_id,
                        taker_wallet = excluded.taker_wallet,
                        settlement_status = excluded.settlement_status,
                        input_asset = excluded.input_asset,
                        input_amount_raw = excluded.input_amount_raw,
                        output_asset = excluded.output_asset,
                        output_amount_raw = excluded.output_amount_raw,
                        execution_path_json = excluded.execution_path_json,
                        trade_json = excluded.trade_json,
                        created_at = excluded.created_at,
                        updated_at = excluded.updated_at",
                    params![
                        trade.trade_id.to_string(),
                        trade.quote_id.to_string(),
                        trade.run_id.to_string(),
                        trade.taker_wallet.as_ref().map(WalletAddress::as_str),
                        settlement_status_label(trade.settlement_status),
                        trade.input_amount.asset.as_str(),
                        trade.input_amount.amount_raw.as_u64().to_string(),
                        trade.output_amount.asset.as_str(),
                        trade.output_amount.amount_raw.as_u64().to_string(),
                        execution_path_json,
                        trade_json,
                        created_at,
                        updated_at,
                    ],
                )
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            transaction
                .execute(
                    "DELETE FROM runtime_trade_signatures WHERE trade_id = ?1",
                    params![trade.trade_id.to_string()],
                )
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            for signature in &trade.tx_signature_kinds {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO runtime_trade_signatures
                         (id, trade_id, signature_kind, signature, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            Uuid::now_v7().to_string(),
                            trade.trade_id.to_string(),
                            trade_signature_kind_label(signature.kind),
                            signature.signature.as_str(),
                            updated_at,
                        ],
                    )
                    .map_err(crate::adapters::persistence::db::sqlite_error)?;
            }
            transaction
                .commit()
                .map_err(crate::adapters::persistence::db::sqlite_error)
        })
    }

    /// Load a terminal trade by id.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when SQLite or deserialization fails.
    pub fn trade(&self, trade_id: TradeId) -> AppResult<Option<RuntimeTrade>> {
        self.db.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT trade_json FROM runtime_trades WHERE trade_id = ?1",
                    params![trade_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(crate::adapters::persistence::db::sqlite_error)?
                .map(|json| runtime_trade_from_json(&json))
                .transpose()
        })
    }

    /// Load every terminal trade.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when SQLite or deserialization fails.
    pub fn load_trades(&self) -> AppResult<Vec<RuntimeTrade>> {
        self.db.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT trade_json FROM runtime_trades ORDER BY created_at DESC")
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            let mut trades = Vec::new();
            for row in rows {
                trades.push(runtime_trade_from_json(
                    &row.map_err(crate::adapters::persistence::db::sqlite_error)?,
                )?);
            }
            Ok(trades)
        })
    }

    /// Persist an active browser-wallet settlement.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when SQLite or serialization fails.
    fn save_wallet_settlement(&self, state: &WalletSettlementState) -> AppResult<()> {
        let state_json = serde_json::to_string(state).map_err(|error| {
            AppError::persistence(format!("serialize wallet settlement state: {error}"))
        })?;
        let created_at = format_runtime_timestamp(state.created_at)?;
        let updated_at = format_runtime_timestamp(OffsetDateTime::now_utc())?;
        self.db.with_connection_mut(|connection| {
            connection
                .execute(
                    "INSERT INTO runtime_wallet_settlements (
                        trade_id, quote_id, run_id, taker_wallet, settlement_phase, expires_at,
                        input_asset, input_amount_raw, output_asset, output_amount_raw,
                        state_json, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(trade_id) DO UPDATE SET
                        quote_id = excluded.quote_id,
                        run_id = excluded.run_id,
                        taker_wallet = excluded.taker_wallet,
                        settlement_phase = excluded.settlement_phase,
                        expires_at = excluded.expires_at,
                        input_asset = excluded.input_asset,
                        input_amount_raw = excluded.input_amount_raw,
                        output_asset = excluded.output_asset,
                        output_amount_raw = excluded.output_amount_raw,
                        state_json = excluded.state_json,
                        created_at = excluded.created_at,
                        updated_at = excluded.updated_at",
                    params![
                        state.settlement.terms.trade_id.to_string(),
                        state.quote.quote_id.to_string(),
                        state.run_id.to_string(),
                        state.taker_wallet.as_str(),
                        settlement_phase_label(state.settlement.phase),
                        format_runtime_timestamp(state.settlement.terms.expires_at)?,
                        state.quote.input_amount.asset.as_str(),
                        state.quote.input_amount.amount_raw.as_u64().to_string(),
                        state.quote.output_amount.asset.as_str(),
                        state.quote.output_amount.amount_raw.as_u64().to_string(),
                        state_json,
                        created_at,
                        updated_at,
                    ],
                )
                .map(|_| ())
                .map_err(crate::adapters::persistence::db::sqlite_error)
        })
    }

    /// Delete an active wallet settlement after terminalization.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when SQLite fails.
    fn delete_wallet_settlement(&self, trade_id: TradeId) -> AppResult<()> {
        self.db.with_connection_mut(|connection| {
            connection
                .execute(
                    "DELETE FROM runtime_wallet_settlements WHERE trade_id = ?1",
                    params![trade_id.to_string()],
                )
                .map(|_| ())
                .map_err(crate::adapters::persistence::db::sqlite_error)
        })
    }

    /// Load active browser-wallet settlements.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when SQLite or deserialization fails.
    fn load_wallet_settlements(&self) -> AppResult<Vec<WalletSettlementState>> {
        self.db.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT state_json FROM runtime_wallet_settlements ORDER BY created_at DESC",
                )
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(crate::adapters::persistence::db::sqlite_error)?;
            let mut states = Vec::new();
            for row in rows {
                let json = row.map_err(crate::adapters::persistence::db::sqlite_error)?;
                states.push(
                    serde_json::from_str::<WalletSettlementState>(&json).map_err(|error| {
                        AppError::persistence(format!(
                            "deserialize wallet settlement state: {error}"
                        ))
                    })?,
                );
            }
            Ok(states)
        })
    }

    fn consume_pnl(&self, event: &RuntimeEvent) -> AppResult<usize> {
        match event {
            RuntimeEvent::Swap(SwapEvent::PriceObserved { price, .. }) => {
                let mut price_book = self
                    .price_book
                    .lock()
                    .map_err(|_| AppError::persistence("P&L price book mutex poisoned"))?;
                price_book.record_price(price);
                Ok(0)
            }
            RuntimeEvent::Swap(SwapEvent::Quoted { metadata, quote }) => {
                let Some(fee) = &quote.estimated_fee else {
                    return Ok(0);
                };
                let price_book = self
                    .price_book
                    .lock()
                    .map_err(|_| AppError::persistence("P&L price book mutex poisoned"))?;
                let fee_value = price_book.estimate_usdc(&self.registry, fee)?;
                let estimate = PnlEstimate::new(
                    metadata.event_id,
                    "swap_quote_estimated_fee",
                    metadata.event_id,
                    PnlCategory::RebalanceCost,
                    fee,
                    -fee_value.abs(),
                    "Jupiter estimated fee",
                    metadata.occurred_at,
                );
                PnlRepository::new(self.db.as_ref())
                    .save_estimate(&estimate)
                    .map(usize::from)
            }
            _ => Ok(0),
        }
    }
}

fn runtime_trade_from_json(json: &str) -> AppResult<RuntimeTrade> {
    serde_json::from_str(json)
        .map_err(|error| AppError::persistence(format!("deserialize runtime trade: {error}")))
}

fn format_runtime_timestamp(timestamp: OffsetDateTime) -> AppResult<String> {
    timestamp
        .format(&Rfc3339)
        .map_err(|error| AppError::persistence(format!("format timestamp: {error}")))
}

fn settlement_status_label(status: SettlementStatus) -> &'static str {
    match status {
        SettlementStatus::Pending => "pending",
        SettlementStatus::Initiated => "initiated",
        SettlementStatus::Redeemed => "redeemed",
        SettlementStatus::Refunded => "refunded",
        SettlementStatus::Failed => "failed",
    }
}

fn settlement_phase_label(phase: SettlementPhase) -> &'static str {
    match phase {
        SettlementPhase::Pending => "pending",
        SettlementPhase::Started => "started",
        SettlementPhase::TakerLocked => "taker_locked",
        SettlementPhase::MakerLocked => "maker_locked",
        SettlementPhase::TakerRedeemed => "taker_redeemed",
        SettlementPhase::Complete => "complete",
        SettlementPhase::Refunded => "refunded",
        SettlementPhase::Failed => "failed",
    }
}

fn trade_signature_kind_label(kind: TradeSignatureKind) -> &'static str {
    match kind {
        TradeSignatureKind::TakerLock => "taker_lock",
        TradeSignatureKind::TakerRedeem => "taker_redeem",
        TradeSignatureKind::TakerRefund => "taker_refund",
        TradeSignatureKind::MakerLock => "maker_lock",
        TradeSignatureKind::MakerRedeem => "maker_redeem",
        TradeSignatureKind::MakerRefund => "maker_refund",
        TradeSignatureKind::GatewayBurn => "gateway_burn",
        TradeSignatureKind::GatewayMint => "gateway_mint",
        TradeSignatureKind::JupiterSwap => "jupiter_swap",
    }
}

fn load_terminal_trades_for_startup(
    persistence: &RuntimePersistence,
) -> HashMap<TradeId, RuntimeTrade> {
    match persistence.load_trades() {
        Ok(trades) => trades
            .into_iter()
            .map(|trade| (trade.trade_id, trade))
            .collect(),
        Err(error) => {
            tracing::warn!(?error, "failed to load persisted terminal trades");
            HashMap::new()
        }
    }
}

fn load_active_wallet_settlements_for_startup(
    persistence: &RuntimePersistence,
) -> HashMap<TradeId, WalletSettlementState> {
    let now = OffsetDateTime::now_utc();
    let mut active = HashMap::new();
    let states = match persistence.load_wallet_settlements() {
        Ok(states) => states,
        Err(error) => {
            tracing::warn!(?error, "failed to load active wallet settlements");
            return active;
        }
    };

    for mut state in states {
        let trade_id = state.settlement.terms.trade_id;
        if state.tx_signature_kinds.is_empty() && state.settlement.terms.expires_at <= now {
            let failure = state
                .settlement
                .fail(state.run_id, "quote_expired_before_lock");
            let _ = failure;
            let trade = runtime_trade_from_wallet_state(&state, SettlementStatus::Failed);
            if let Err(error) = persistence.save_trade(&trade) {
                tracing::warn!(?error, %trade_id, "failed to persist expired wallet settlement");
            }
            if let Err(error) = persistence.delete_wallet_settlement(trade_id) {
                tracing::warn!(?error, %trade_id, "failed to delete expired wallet settlement");
            }
            continue;
        }
        active.insert(trade_id, state);
    }

    active
}

fn runtime_trade_from_wallet_state(
    state: &WalletSettlementState,
    status: SettlementStatus,
) -> RuntimeTrade {
    RuntimeTrade {
        quote_id: state.quote.quote_id,
        trade_id: state.settlement.terms.trade_id,
        run_id: state.run_id,
        taker_wallet: Some(state.taker_wallet.clone()),
        expires_at: Some(state.settlement.terms.expires_at),
        settlement_status: status,
        tx_signatures: state.tx_signatures.clone(),
        tx_signature_kinds: state.tx_signature_kinds.clone(),
        created_at: state.created_at,
        input_amount: state.quote.input_amount.clone(),
        output_amount: state.quote.output_amount.clone(),
        execution_path: state.quote.execution_path,
    }
}

fn coarse_status_for_wallet_phase(phase: SettlementPhase) -> SettlementStatus {
    match phase {
        SettlementPhase::Complete => SettlementStatus::Redeemed,
        SettlementPhase::Refunded => SettlementStatus::Refunded,
        SettlementPhase::Failed => SettlementStatus::Failed,
        SettlementPhase::TakerLocked
        | SettlementPhase::MakerLocked
        | SettlementPhase::TakerRedeemed => SettlementStatus::Initiated,
        SettlementPhase::Pending | SettlementPhase::Started => SettlementStatus::Pending,
    }
}

fn has_signature_kind(state: &WalletSettlementState, kind: TradeSignatureKind) -> bool {
    state
        .tx_signature_kinds
        .iter()
        .any(|signature| signature.kind == kind)
}

fn signature_for_kind(
    state: &WalletSettlementState,
    kind: TradeSignatureKind,
) -> Option<TxSignature> {
    state
        .tx_signature_kinds
        .iter()
        .find(|signature| signature.kind == kind)
        .map(|signature| TxSignature::new(signature.signature.clone()))
}

fn wallet_refund_available(state: &WalletSettlementState) -> bool {
    state.settlement.terms.expires_at <= OffsetDateTime::now_utc()
        && has_signature_kind(state, TradeSignatureKind::TakerLock)
        && !has_signature_kind(state, TradeSignatureKind::TakerRedeem)
        && !has_signature_kind(state, TradeSignatureKind::TakerRefund)
}

fn wallet_prelock_expired(state: &WalletSettlementState) -> bool {
    state.settlement.terms.expires_at <= OffsetDateTime::now_utc()
        && !has_signature_kind(state, TradeSignatureKind::TakerLock)
}

fn ensure_wallet_refundable(state: &WalletSettlementState) -> AppResult<()> {
    if !has_signature_kind(state, TradeSignatureKind::TakerLock) {
        return Err(AppError::validation(
            "cannot refund before taker funds are locked",
        ));
    }
    if has_signature_kind(state, TradeSignatureKind::TakerRedeem) {
        return Err(AppError::validation(
            "cannot refund after the taker redeem has been submitted",
        ));
    }
    if state.settlement.terms.expires_at > OffsetDateTime::now_utc() {
        return Err(AppError::validation(
            "settlement is not refundable until the quote expiry passes",
        ));
    }
    Ok(())
}

/// Event-driven runtime flow coordinator.
#[derive(Clone)]
pub struct RuntimeOrchestrator {
    app_state: AppState,
    adapters: RuntimeAdapters,
    quotes: Arc<RwLock<HashMap<QuoteId, FirmQuote>>>,
    trades: Arc<RwLock<HashMap<TradeId, RuntimeTrade>>>,
    wallet_settlements: Arc<RwLock<HashMap<TradeId, WalletSettlementState>>>,
    automation_state: Arc<RwLock<AutomationState>>,
    /// Serializes `refresh_inventory_and_automation` calls. The trade-driven
    /// `accept_quote` path and each always-on worker share this lock so that
    /// concurrent firings cannot double-spend the cumulative cap or stack
    /// overlapping Jupiter/Gateway adapter actions.
    automation_lock: Arc<tokio::sync::Mutex<()>>,
    usd_prices: Arc<RwLock<BTreeMap<AssetId, Decimal>>>,
    persistence: Option<Arc<RuntimePersistence>>,
    last_accepted_quote: Arc<RwLock<Option<QuoteId>>>,
    demo_taker_wallet: Option<crate::domain::types::WalletAddress>,
    allow_local_taker_settlement: bool,
}

impl RuntimeOrchestrator {
    /// Create an orchestrator over an already bootstrapped runtime state.
    #[must_use]
    pub fn new(
        app_state: AppState,
        adapters: RuntimeAdapters,
        options: RuntimeOrchestratorOptions,
    ) -> Self {
        let automation_state = AutomationState {
            cumulative_spend_usd: options.initial_automation_spend_usd,
            ..AutomationState::default()
        };
        let usd_prices = BTreeMap::from([(AssetId::from("USDC"), Decimal::ONE)]);

        Self {
            app_state,
            adapters,
            quotes: Arc::new(RwLock::new(HashMap::new())),
            trades: Arc::new(RwLock::new(HashMap::new())),
            wallet_settlements: Arc::new(RwLock::new(HashMap::new())),
            automation_state: Arc::new(RwLock::new(automation_state)),
            automation_lock: Arc::new(tokio::sync::Mutex::new(())),
            usd_prices: Arc::new(RwLock::new(usd_prices)),
            persistence: None,
            last_accepted_quote: Arc::new(RwLock::new(None)),
            demo_taker_wallet: options.demo_taker_wallet,
            allow_local_taker_settlement: options.allow_local_taker_settlement,
        }
    }

    /// Create an orchestrator with durable event persistence enabled.
    #[must_use]
    pub fn new_with_persistence(
        app_state: AppState,
        adapters: RuntimeAdapters,
        persistence: Arc<RuntimePersistence>,
        options: RuntimeOrchestratorOptions,
    ) -> Self {
        let automation_state = AutomationState {
            cumulative_spend_usd: options.initial_automation_spend_usd,
            ..AutomationState::default()
        };
        let usd_prices = BTreeMap::from([(AssetId::from("USDC"), Decimal::ONE)]);
        let trades = load_terminal_trades_for_startup(&persistence);
        let wallet_settlements = load_active_wallet_settlements_for_startup(&persistence);

        Self {
            app_state,
            adapters,
            quotes: Arc::new(RwLock::new(HashMap::new())),
            trades: Arc::new(RwLock::new(trades)),
            wallet_settlements: Arc::new(RwLock::new(wallet_settlements)),
            automation_state: Arc::new(RwLock::new(automation_state)),
            automation_lock: Arc::new(tokio::sync::Mutex::new(())),
            usd_prices: Arc::new(RwLock::new(usd_prices)),
            persistence: Some(persistence),
            last_accepted_quote: Arc::new(RwLock::new(None)),
            demo_taker_wallet: options.demo_taker_wallet,
            allow_local_taker_settlement: options.allow_local_taker_settlement,
        }
    }

    /// Borrow the application config.
    #[must_use]
    pub const fn config(&self) -> &AppConfig {
        self.app_state.config()
    }

    /// Return the shared runtime projection handle.
    #[must_use]
    pub fn runtime(&self) -> RuntimeHandle {
        self.app_state.runtime()
    }

    /// Return the runtime invocation identifier.
    #[must_use]
    pub const fn run_id(&self) -> crate::domain::types::RuntimeRunId {
        self.app_state.run_id()
    }

    /// Return the durable persistence handle if one is wired. Used by the
    /// reconciliation worker to read ledger state and post adjustments
    /// directly.
    #[must_use]
    pub fn persistence_handle(&self) -> Option<Arc<RuntimePersistence>> {
        self.persistence.clone()
    }

    /// Return the durable runtime persistence layer when wired. Alias for
    /// `persistence_handle` used by the public read endpoints.
    #[must_use]
    pub fn persistence(&self) -> Option<Arc<RuntimePersistence>> {
        self.persistence.clone()
    }

    fn persist_trade(&self, trade: &RuntimeTrade) -> AppResult<()> {
        if let Some(persistence) = &self.persistence {
            persistence.save_trade(trade)?;
        }
        Ok(())
    }

    fn persist_wallet_settlement(&self, state: &WalletSettlementState) -> AppResult<()> {
        if let Some(persistence) = &self.persistence {
            persistence.save_wallet_settlement(state)?;
        }
        Ok(())
    }

    async fn persist_and_store_wallet_settlement(
        &self,
        state: &WalletSettlementState,
    ) -> AppResult<()> {
        self.persist_wallet_settlement(state)?;
        self.wallet_settlements
            .write()
            .await
            .insert(state.settlement.terms.trade_id, state.clone());
        Ok(())
    }

    fn delete_persisted_wallet_settlement(&self, trade_id: TradeId) -> AppResult<()> {
        if let Some(persistence) = &self.persistence {
            persistence.delete_wallet_settlement(trade_id)?;
        }
        Ok(())
    }

    /// Read live wallet balances through the configured `BalanceReader` port.
    ///
    /// # Errors
    ///
    /// Returns adapter errors (RPC, decoding, etc.).
    pub async fn balances(&self, wallet: WalletRole) -> AppResult<BalanceSnapshot> {
        self.adapters.balance_reader.balances(wallet).await
    }

    /// Read the current Gateway balance for an asset through the Gateway
    /// adapter port. Used by the reconciliation worker.
    ///
    /// # Errors
    ///
    /// Returns adapter errors (Gateway connectivity, decoding, etc.).
    pub async fn gateway_balance(
        &self,
        asset: AssetId,
    ) -> AppResult<crate::domain::types::GatewayReceipt> {
        self.adapters.gateway_client.balance(asset).await
    }

    /// Return the asset registry from the durable runtime persistence layer
    /// when wired, falling back to a registry built from the current config
    /// otherwise.
    #[must_use]
    pub fn asset_registry(&self) -> AssetRegistry {
        self.persistence.as_ref().map_or_else(
            || AssetRegistry::from_config(self.app_state.config()),
            |persistence| persistence.asset_registry().clone(),
        )
    }

    /// Request a firm quote and store it only when risk accepts the RFQ.
    ///
    /// # Errors
    ///
    /// Returns balance-reader or event-publish errors. Risk rejections are
    /// returned as successful [`RfqResponse::Rejected`] values.
    pub async fn request_rfq(&self, request: RfqRequest) -> AppResult<RfqResponse> {
        let now = OffsetDateTime::now_utc();
        self.expire_stored_quotes(now).await?;

        let balance_snapshot = self
            .adapters
            .balance_reader
            .balances(WalletRole::Maker)
            .await?;
        self.publish(RuntimeEvent::Inventory(InventoryEvent::Snapshot {
            metadata: EventMetadata::new(self.app_state.run_id()),
            snapshot: balance_snapshot.clone(),
        }))
        .await?;

        // Quoteability reads from the ledger's working_custody when persistence
        // is wired (the operational source of truth). The live wallet snapshot
        // is still emitted above for visibility/reconciliation but no longer
        // gates the RFQ. When no persistence is wired (legacy harness tests),
        // fall back to the live snapshot so behaviour is unchanged.
        let mut inventory = self
            .ledger_quote_inventory(&balance_snapshot)?
            .unwrap_or_else(|| {
                quote_inventory_from_balance(self.app_state.config(), &balance_snapshot)
            });
        // T9: if the output asset's working_custody is short, the maker may
        // still source the fill via the Circle Gateway path. Augment the
        // output-asset supply with FREE Gateway USDC (gateway minus
        // gateway_reserved), converted into output-asset units via the
        // reference price. The risk gate then evaluates against the combined
        // inventory + Gateway-derived supply.
        self.augment_inventory_with_gateway_supply(&request, &mut inventory)
            .await?;
        let asset_usd_prices = self.usd_prices_for_rfq(&request).await?;
        let context = RfqContext::from_config_with_usd_prices(
            self.app_state.config(),
            inventory,
            asset_usd_prices,
        );
        let outcome = request_quote(
            request,
            &context,
            self.adapters.price_provider.as_ref(),
            self.app_state.run_id(),
            now,
        )
        .await;

        let mut response = outcome.response;
        for event in outcome.events {
            self.publish(event).await?;
        }

        if let RfqResponse::Accepted(quote) = &mut response {
            self.record_usd_price(&quote.reference_price).await;
            self.publish(RuntimeEvent::Swap(SwapEvent::PriceObserved {
                metadata: EventMetadata::new(self.app_state.run_id()),
                price: quote.reference_price.clone(),
            }))
            .await?;
            // Resolve the execution path before storing. The default from
            // `request_quote` is `InventoryToInventory`; when persistence is
            // wired and the working_custody for the output asset cannot cover
            // the requested output but free Gateway USDC can, switch to
            // `GatewayToDex`. Insufficient liquidity here would have already
            // been caught by the inventory gate; we only differentiate the two
            // accepted paths.
            quote.execution_path = self.resolve_execution_path(quote).await?;
            self.quotes
                .write()
                .await
                .insert(quote.quote_id, *quote.clone());
            *self.last_accepted_quote.write().await = Some(quote.quote_id);
        }

        Ok(response)
    }

    async fn usd_prices_for_rfq(&self, request: &RfqRequest) -> AppResult<Vec<(AssetId, Decimal)>> {
        let mut prices = self.usd_prices.read().await.clone();
        prices.insert(AssetId::from("USDC"), Decimal::ONE);

        let assets = &self.app_state.config().assets.supported;
        let input_asset = assets
            .iter()
            .find(|asset| asset.enabled && asset.mint == request.input_mint);
        let output_asset = assets
            .iter()
            .find(|asset| asset.enabled && asset.mint == request.output_mint);

        if let Some(asset) = input_asset {
            self.ensure_usd_price(&mut prices, asset).await;
        }
        if let Some(asset) = output_asset {
            self.ensure_usd_price(&mut prices, asset).await;
        }

        let mut stored_prices = self.usd_prices.write().await;
        for (asset, price) in prices {
            stored_prices.insert(asset, price);
        }
        Ok(stored_prices
            .iter()
            .map(|(asset, price)| (asset.clone(), *price))
            .collect())
    }

    async fn ensure_usd_price(&self, prices: &mut BTreeMap<AssetId, Decimal>, asset: &AssetConfig) {
        let usdc = AssetId::from("USDC");
        if asset.id == usdc || prices.contains_key(&asset.id) {
            return;
        }

        let asset_to_usdc = crate::domain::types::AssetPair::new(asset.id.clone(), usdc.clone());
        if let Ok(price) = self
            .adapters
            .price_provider
            .reference_price(asset_to_usdc)
            .await
        {
            if price.output_per_input > Decimal::ZERO {
                prices.insert(asset.id.clone(), price.output_per_input);
                return;
            }
        }

        let usdc_to_asset = crate::domain::types::AssetPair::new(usdc, asset.id.clone());
        if let Ok(price) = self
            .adapters
            .price_provider
            .reference_price(usdc_to_asset)
            .await
            && price.output_per_input > Decimal::ZERO
            && let Some(inverse) = Decimal::ONE.checked_div(price.output_per_input)
        {
            prices.insert(asset.id.clone(), inverse);
        }
    }

    /// Resolve the [`ExecutionPath`] for an accepted quote. Walks the same
    /// rules as [`crate::application::rfq::select_execution_path`] using the
    /// ledger's `working_custody` and FREE Gateway balance.
    ///
    /// When no persistence is wired (legacy harness tests) the path defaults
    /// to `InventoryToInventory` — the existing behaviour before this method
    /// existed.
    async fn resolve_execution_path(&self, quote: &FirmQuote) -> AppResult<ExecutionPath> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(ExecutionPath::InventoryToInventory);
        };

        let output_asset = &quote.output_amount.asset;
        let requested = quote.output_amount.amount_raw.as_u64();
        let working = persistence.working_custody_balance(output_asset)?.as_u64();

        if working >= requested {
            return Ok(ExecutionPath::InventoryToInventory);
        }

        // Translate FREE Gateway USDC into the output asset's raw units.
        let usdc = AssetId::from("USDC");
        let free_gateway_usdc = persistence.gateway_free_balance(&usdc)?;
        if free_gateway_usdc.is_zero() {
            // Inventory gate would normally have rejected the quote, but
            // request_quote ran against the augmented inventory so we may end
            // up here. Default to inventory and let downstream lifecycle
            // surface any shortfall via failure events.
            return Ok(ExecutionPath::InventoryToInventory);
        }
        let gateway_equivalent = if output_asset == &usdc {
            free_gateway_usdc.as_u64()
        } else {
            let usdc_decimals = self
                .app_state
                .config()
                .assets
                .supported
                .iter()
                .find(|asset| asset.enabled && asset.id == usdc)
                .map_or(6, |asset| asset.decimals);
            let output_decimals = self
                .app_state
                .config()
                .assets
                .supported
                .iter()
                .find(|asset| asset.enabled && asset.id == *output_asset)
                .map_or(9, |asset| asset.decimals);
            let pair = crate::domain::types::AssetPair::new(usdc.clone(), output_asset.clone());
            let Ok(reference_price) = self.adapters.price_provider.reference_price(pair).await
            else {
                return Ok(ExecutionPath::InventoryToInventory);
            };
            convert_usdc_to_asset_raw(
                free_gateway_usdc,
                usdc_decimals,
                reference_price.output_per_input,
                output_decimals,
            )
        };

        if gateway_equivalent >= requested {
            Ok(ExecutionPath::GatewayToDex)
        } else {
            // Neither strictly covers — but the quote was already accepted by
            // the augmented gate. Default to inventory to preserve existing
            // behaviour; the consumer may downstream-fail.
            Ok(ExecutionPath::InventoryToInventory)
        }
    }

    async fn expire_stored_quotes(&self, now: OffsetDateTime) -> AppResult<()> {
        let expired_quote_ids = {
            let mut quotes = self.quotes.write().await;
            let expired_quote_ids = quotes
                .iter()
                .filter_map(|(quote_id, quote)| (quote.expires_at <= now).then_some(*quote_id))
                .collect::<Vec<_>>();

            for quote_id in &expired_quote_ids {
                quotes.remove(quote_id);
            }

            expired_quote_ids
        };

        for quote_id in expired_quote_ids {
            self.publish(RuntimeEvent::Quote(QuoteEvent::Expired {
                metadata: EventMetadata::new(self.app_state.run_id()),
                quote_id,
            }))
            .await?;
        }

        Ok(())
    }

    /// Accept a stored quote, run the two-sided HTLC flow, refresh inventory,
    /// and submit capped post-settlement automation decisions.
    ///
    /// # Errors
    ///
    /// Returns validation errors for unknown/expired quotes and adapter errors
    /// for failed settlement steps. Automation failures are emitted as events
    /// and do not fail an otherwise completed trade.
    pub async fn accept_quote(&self, quote_id: QuoteId) -> AppResult<RuntimeTrade> {
        if !self.allow_local_taker_settlement {
            self.ensure_quote_known(quote_id).await?;
            return Err(AppError::unsupported(
                "server-side taker settlement is disabled for the web app; use wallet settlement endpoints instead",
            ));
        }

        let quote = self.take_quote(quote_id).await?;
        let trade_id = self.accept_quote_for_trade(&quote).await?;
        let (tx_signatures, tx_signature_kinds) = self.settle_quote(&quote, trade_id).await?;
        let trade = RuntimeTrade {
            quote_id,
            trade_id,
            run_id: self.app_state.run_id(),
            taker_wallet: Some(quote.htlc_terms.taker_wallet.clone()),
            expires_at: Some(quote.htlc_terms.expires_at),
            settlement_status: SettlementStatus::Redeemed,
            tx_signatures,
            tx_signature_kinds,
            created_at: OffsetDateTime::now_utc(),
            input_amount: quote.input_amount.clone(),
            output_amount: quote.output_amount.clone(),
            execution_path: quote.execution_path,
        };
        self.persist_trade(&trade)?;
        self.trades.write().await.insert(trade_id, trade.clone());

        self.refresh_inventory_and_automation().await?;

        Ok(trade)
    }

    async fn ensure_quote_known(&self, quote_id: QuoteId) -> AppResult<()> {
        if self.quotes.read().await.contains_key(&quote_id) {
            Ok(())
        } else {
            Err(AppError::validation(format!(
                "unknown or rejected quote {quote_id}"
            )))
        }
    }

    async fn take_quote(&self, quote_id: QuoteId) -> AppResult<FirmQuote> {
        self.quotes
            .write()
            .await
            .remove(&quote_id)
            .ok_or_else(|| AppError::validation(format!("unknown or rejected quote {quote_id}")))
    }

    async fn accept_quote_for_trade(&self, quote: &FirmQuote) -> AppResult<TradeId> {
        match accept_quote_for_settlement(quote, self.app_state.run_id(), OffsetDateTime::now_utc())
        {
            QuoteAcceptance::Accepted { trade_id, event } => {
                self.publish(event).await?;
                Ok(trade_id)
            }
            QuoteAcceptance::Expired { decision: _, event } => {
                self.publish(event).await?;
                // Emit a settlement failure event so downstream consumers can
                // observe the repricing-failed outcome under a stable reason
                // string; record a Failed RuntimeTrade so operator views can
                // surface the cancelled trade alongside successful ones.
                let trade_id = TradeId::generate();
                self.publish(RuntimeEvent::Settlement(SettlementEvent::Failed {
                    metadata: EventMetadata::new(self.app_state.run_id()),
                    trade_id,
                    reason: "repricing_failed".to_owned(),
                }))
                .await?;
                let trade = RuntimeTrade {
                    quote_id: quote.quote_id,
                    trade_id,
                    run_id: self.app_state.run_id(),
                    taker_wallet: Some(quote.htlc_terms.taker_wallet.clone()),
                    expires_at: Some(quote.htlc_terms.expires_at),
                    settlement_status: SettlementStatus::Failed,
                    tx_signatures: Vec::new(),
                    tx_signature_kinds: Vec::new(),
                    created_at: time::OffsetDateTime::now_utc(),
                    input_amount: quote.input_amount.clone(),
                    output_amount: quote.output_amount.clone(),
                    execution_path: quote.execution_path,
                };
                self.persist_trade(&trade)?;
                self.trades.write().await.insert(trade_id, trade);
                Err(AppError::validation(format!(
                    "quote {} expired",
                    quote.quote_id
                )))
            }
        }
    }

    async fn settle_quote(
        &self,
        quote: &FirmQuote,
        trade_id: TradeId,
    ) -> AppResult<(Vec<TxSignature>, Vec<TradeSignature>)> {
        let secret = generate_secret();
        let preimage = hex::encode(secret);
        let terms = SettlementTerms {
            quote_id: quote.quote_id,
            trade_id,
            taker_input: quote.htlc_terms.maker_receive.clone(),
            maker_output: quote.htlc_terms.maker_pay.clone(),
            taker_wallet: WalletRole::Taker,
            maker_wallet: WalletRole::Maker,
            secret_hash: hex::encode(hash_secret(&secret)),
            expires_at: quote.htlc_terms.expires_at,
        };
        let mut settlement = TwoSidedSettlement::new(terms);
        let mut tx_signatures = Vec::new();
        let mut tx_signature_kinds = Vec::new();

        self.run_settlement_locks(
            &mut settlement,
            quote,
            &mut tx_signatures,
            &mut tx_signature_kinds,
        )
        .await?;
        self.run_settlement_redeems(
            &mut settlement,
            quote,
            &mut tx_signatures,
            &mut tx_signature_kinds,
            preimage,
        )
        .await?;
        self.publish(RuntimeEvent::Settlement(SettlementEvent::StatusChanged {
            metadata: EventMetadata::new(self.app_state.run_id()),
            trade_id,
            status: SettlementStatus::Redeemed,
        }))
        .await?;

        Ok((tx_signatures, tx_signature_kinds))
    }

    /// Start connected-wallet settlement for an accepted quote.
    ///
    /// # Errors
    ///
    /// Returns validation errors for unknown/expired quotes, wallet mismatch,
    /// or adapter errors while building the unsigned taker-lock transaction.
    pub async fn start_wallet_settlement(
        &self,
        quote_id: QuoteId,
        taker_wallet: WalletAddress,
        secret_hash: String,
    ) -> AppResult<WalletSettlementStart> {
        let known_quote = self
            .quotes
            .read()
            .await
            .get(&quote_id)
            .cloned()
            .ok_or_else(|| AppError::validation(format!("unknown or rejected quote {quote_id}")))?;
        if known_quote.htlc_terms.taker_wallet != taker_wallet {
            return Err(AppError::validation(
                "connected wallet does not match the RFQ taker wallet",
            ));
        }
        let quote = self.take_quote(quote_id).await?;
        if quote.htlc_terms.taker_wallet != taker_wallet {
            return Err(AppError::validation(
                "connected wallet does not match the RFQ taker wallet",
            ));
        }

        let trade_id = self.accept_quote_for_trade(&quote).await?;
        let maker_wallet = self
            .adapters
            .htlc_client
            .wallet_address(WalletRole::Maker)
            .await?;
        let terms = SettlementTerms {
            quote_id,
            trade_id,
            taker_input: quote.htlc_terms.maker_receive.clone(),
            maker_output: quote.htlc_terms.maker_pay.clone(),
            taker_wallet: WalletRole::Taker,
            maker_wallet: WalletRole::Maker,
            secret_hash,
            expires_at: quote.htlc_terms.expires_at,
        };
        let mut settlement = TwoSidedSettlement::new(terms);
        let start = settlement.start(self.app_state.run_id());
        self.publish(RuntimeEvent::Settlement(start.event)).await?;

        let taker_lock_request = ExternalHtlcInitiation {
            trade_id,
            funder: taker_wallet.clone(),
            redeemer: maker_wallet,
            amount: quote.htlc_terms.maker_receive.clone(),
            hashlock: settlement.terms.secret_hash.clone(),
            expires_at: settlement.terms.expires_at,
        };
        let taker_lock_transaction = self
            .adapters
            .htlc_client
            .build_external_initiate(taker_lock_request.clone())
            .await?;
        let expires_at = quote.htlc_terms.expires_at;

        self.wallet_settlements.write().await.insert(trade_id, {
            let state = WalletSettlementState {
                quote,
                settlement,
                taker_lock_request,
                taker_wallet,
                tx_signatures: Vec::new(),
                tx_signature_kinds: Vec::new(),
                run_id: self.app_state.run_id(),
                created_at: OffsetDateTime::now_utc(),
            };
            self.persist_wallet_settlement(&state)?;
            state
        });

        Ok(WalletSettlementStart {
            quote_id,
            trade_id,
            taker_lock_transaction,
            expires_at,
        })
    }

    /// Record the browser-submitted taker lock, then submit the maker lock.
    ///
    /// # Errors
    ///
    /// Returns validation errors for unknown trades or adapter errors from
    /// signature confirmation and maker lock submission.
    pub async fn record_wallet_taker_lock(
        &self,
        trade_id: TradeId,
        taker_lock_signature: TxSignature,
    ) -> AppResult<WalletTakerLockResult> {
        if let Some(trade) = self.trades.read().await.get(&trade_id).cloned() {
            if trade.tx_signature_kinds.iter().any(|signature| {
                signature.kind == TradeSignatureKind::TakerLock
                    && signature.signature == taker_lock_signature.as_str()
            }) {
                return Ok(WalletTakerLockResult {
                    trade_id,
                    settlement_status: trade.settlement_status,
                    maker_lock_signature: trade
                        .tx_signature_kinds
                        .iter()
                        .find(|signature| signature.kind == TradeSignatureKind::MakerLock)
                        .map(|signature| TxSignature::new(signature.signature.clone())),
                    tx_signatures: trade.tx_signatures,
                });
            }
        }
        let mut state = self.wallet_settlement_state(trade_id).await?;
        if wallet_prelock_expired(&state) {
            self.fail_expired_prelock_wallet_settlement(state).await?;
            return Err(AppError::validation(
                "wallet settlement expired before funds were locked",
            ));
        }
        if !has_signature_kind(&state, TradeSignatureKind::TakerLock) {
            let taker_lock = self
                .adapters
                .htlc_client
                .record_external_initiate(state.taker_lock_request.clone(), taker_lock_signature)
                .await?;
            push_trade_signature(
                &mut state.tx_signatures,
                &mut state.tx_signature_kinds,
                &taker_lock,
            );
            let transition = state.settlement.record_taker_lock(
                self.app_state.run_id(),
                taker_lock.signature.as_ref().map(ToString::to_string),
            );
            self.publish(RuntimeEvent::Settlement(transition.event))
                .await?;
            // TODO(v0.2): wire real confirmation polling for the wallet flow.
            // The browser-driven path posts the signed taker tx via
            // `record_external_initiate`, which already confirms and validates
            // the expected HTLC account before returning.
            let confirmation = state
                .settlement
                .confirm_taker_lock(self.app_state.run_id())?;
            self.publish(RuntimeEvent::Settlement(confirmation.event))
                .await?;
            self.persist_and_store_wallet_settlement(&state).await?;
        }

        if !has_signature_kind(&state, TradeSignatureKind::MakerLock) {
            // Gateway-backed paths run real burn/mint (and Jupiter spend slice
            // for non-USDC outputs) before the maker leg, threading on-chain
            // signatures into the wallet settlement state.
            self.ensure_gateway_backed_input_slice(&mut state).await?;

            let maker_lock = self
                .adapters
                .htlc_client
                .initiate_with_external_redeemer(
                    state.settlement.terms.maker_lock_request(),
                    state.taker_wallet.clone(),
                )
                .await?;
            push_trade_signature(
                &mut state.tx_signatures,
                &mut state.tx_signature_kinds,
                &maker_lock,
            );
            let transition = state.settlement.record_maker_lock(
                self.app_state.run_id(),
                maker_lock.signature.as_ref().map(ToString::to_string),
            );
            self.publish(RuntimeEvent::Settlement(transition.event))
                .await?;
            // TODO(v0.2): wire real confirmation polling for the wallet flow.
            // The maker leg uses `initiate_with_external_redeemer` which submits
            // and signs locally; for v1 we treat the submission as confirmed.
            let confirmation = state
                .settlement
                .confirm_maker_lock(self.app_state.run_id())?;
            self.publish(RuntimeEvent::Settlement(confirmation.event))
                .await?;
            self.persist_and_store_wallet_settlement(&state).await?;
        }

        let response = WalletTakerLockResult {
            trade_id,
            settlement_status: SettlementStatus::Initiated,
            maker_lock_signature: signature_for_kind(&state, TradeSignatureKind::MakerLock),
            tx_signatures: state.tx_signatures.clone(),
        };
        Ok(response)
    }

    /// Build the unsigned taker redeem transaction after the browser reveals
    /// the preimage at redeem time.
    ///
    /// # Errors
    ///
    /// Returns validation errors for unknown trades or adapter construction errors.
    pub async fn prepare_wallet_taker_redeem(
        &self,
        trade_id: TradeId,
        preimage: String,
    ) -> AppResult<WalletTakerRedeemPreparation> {
        let state = self.wallet_settlement_state(trade_id).await?;
        let taker_redeem_transaction = self
            .adapters
            .htlc_client
            .build_external_redeem(trade_id, state.taker_wallet, preimage)
            .await?;

        Ok(WalletTakerRedeemPreparation {
            trade_id,
            taker_redeem_transaction,
        })
    }

    /// Record the browser taker redeem, submit maker redeem, and finalize the trade.
    ///
    /// # Errors
    ///
    /// Returns validation errors for unknown trades or adapter errors from
    /// signature confirmation, maker redeem, ledger, or automation.
    pub async fn complete_wallet_taker_redeem(
        &self,
        trade_id: TradeId,
        preimage: String,
        taker_redeem_signature: TxSignature,
    ) -> AppResult<WalletTakerRedeemResult> {
        if let Some(trade) = self.trades.read().await.get(&trade_id).cloned() {
            if trade.settlement_status == SettlementStatus::Redeemed {
                return Ok(WalletTakerRedeemResult {
                    maker_redeem_signature: trade
                        .tx_signature_kinds
                        .iter()
                        .find(|signature| signature.kind == TradeSignatureKind::MakerRedeem)
                        .map(|signature| TxSignature::new(signature.signature.clone())),
                    trade,
                });
            }
        }
        let mut state = self.wallet_settlement_state(trade_id).await?;
        if has_signature_kind(&state, TradeSignatureKind::TakerRedeem)
            && has_signature_kind(&state, TradeSignatureKind::MakerRedeem)
        {
            let trade = runtime_trade_from_wallet_state(&state, SettlementStatus::Redeemed);
            self.persist_trade(&trade)?;
            self.trades.write().await.insert(trade_id, trade.clone());
            self.wallet_settlements.write().await.remove(&trade_id);
            self.delete_persisted_wallet_settlement(trade_id)?;
            return Ok(WalletTakerRedeemResult {
                trade,
                maker_redeem_signature: signature_for_kind(&state, TradeSignatureKind::MakerRedeem),
            });
        }
        self.restore_wallet_settlement_htlc(&state).await?;
        if !has_signature_kind(&state, TradeSignatureKind::TakerRedeem) {
            let taker_redeem = self
                .adapters
                .htlc_client
                .record_external_redeem(trade_id, taker_redeem_signature)
                .await?;
            push_trade_signature(
                &mut state.tx_signatures,
                &mut state.tx_signature_kinds,
                &taker_redeem,
            );
            let transition = state.settlement.record_taker_redeem(
                self.app_state.run_id(),
                taker_redeem.signature.as_ref().map(ToString::to_string),
            );
            self.publish(RuntimeEvent::Settlement(transition.event))
                .await?;
            self.persist_and_store_wallet_settlement(&state).await?;
        }

        if !has_signature_kind(&state, TradeSignatureKind::MakerRedeem) {
            let maker_redeem = self.adapters.htlc_client.redeem(trade_id, preimage).await?;
            push_trade_signature(
                &mut state.tx_signatures,
                &mut state.tx_signature_kinds,
                &maker_redeem,
            );
            let transition = state.settlement.record_maker_redeem(
                self.app_state.run_id(),
                maker_redeem.signature.as_ref().map(ToString::to_string),
            );
            self.publish(RuntimeEvent::Settlement(transition.event))
                .await?;
            self.persist_and_store_wallet_settlement(&state).await?;
        }
        self.publish(RuntimeEvent::Settlement(SettlementEvent::StatusChanged {
            metadata: EventMetadata::new(self.app_state.run_id()),
            trade_id,
            status: SettlementStatus::Redeemed,
        }))
        .await?;

        let trade = RuntimeTrade {
            quote_id: state.quote.quote_id,
            trade_id,
            run_id: state.run_id,
            taker_wallet: Some(state.taker_wallet.clone()),
            expires_at: Some(state.settlement.terms.expires_at),
            settlement_status: SettlementStatus::Redeemed,
            tx_signatures: state.tx_signatures.clone(),
            tx_signature_kinds: state.tx_signature_kinds.clone(),
            created_at: state.created_at,
            input_amount: state.quote.input_amount.clone(),
            output_amount: state.quote.output_amount.clone(),
            execution_path: state.quote.execution_path,
        };
        self.persist_trade(&trade)?;
        self.trades.write().await.insert(trade_id, trade.clone());
        self.wallet_settlements.write().await.remove(&trade_id);
        self.delete_persisted_wallet_settlement(trade_id)?;
        self.refresh_inventory_and_automation().await?;

        Ok(WalletTakerRedeemResult {
            trade,
            maker_redeem_signature: signature_for_kind(&state, TradeSignatureKind::MakerRedeem),
        })
    }

    /// Resume an active browser-wallet settlement after a page or backend restart.
    ///
    /// # Errors
    ///
    /// Returns validation errors for unknown terminal trades or adapter errors
    /// while rebuilding fresh unsigned wallet transactions.
    pub async fn resume_wallet_settlement(
        &self,
        trade_id: TradeId,
    ) -> AppResult<WalletSettlementResume> {
        let state = self.wallet_settlement_state(trade_id).await?;
        if wallet_prelock_expired(&state) {
            self.fail_expired_prelock_wallet_settlement(state).await?;
            return Err(AppError::validation(
                "wallet settlement expired before funds were locked",
            ));
        }
        self.restore_wallet_settlement_htlc(&state).await?;
        let taker_lock_transaction = if has_signature_kind(&state, TradeSignatureKind::TakerLock) {
            None
        } else {
            Some(
                self.adapters
                    .htlc_client
                    .build_external_initiate(state.taker_lock_request.clone())
                    .await?,
            )
        };
        let taker_refund_transaction = if wallet_refund_available(&state) {
            Some(
                self.adapters
                    .htlc_client
                    .build_external_refund(trade_id, state.taker_wallet.clone())
                    .await?,
            )
        } else {
            None
        };

        Ok(WalletSettlementResume {
            trade_id,
            quote_id: state.quote.quote_id,
            run_id: state.run_id,
            settlement_phase: state.settlement.phase,
            settlement_status: coarse_status_for_wallet_phase(state.settlement.phase),
            taker_lock_transaction,
            taker_redeem_transaction: None,
            taker_refund_transaction,
            tx_signatures: state.tx_signatures.clone(),
            tx_signature_kinds: state.tx_signature_kinds.clone(),
            expires_at: state.settlement.terms.expires_at,
        })
    }

    /// Abandon a pre-lock browser-wallet settlement.
    ///
    /// # Errors
    ///
    /// Returns conflict once any on-chain signature exists.
    pub async fn abandon_wallet_settlement(
        &self,
        trade_id: TradeId,
        secret_hash: String,
    ) -> AppResult<RuntimeTrade> {
        let mut state = self.wallet_settlement_state(trade_id).await?;
        if state.settlement.terms.secret_hash != secret_hash {
            return Err(AppError::validation(
                "secret_hash does not match the wallet settlement",
            ));
        }
        if !state.tx_signature_kinds.is_empty() {
            return Err(AppError::conflict(
                "cannot start over after funds are on-chain; continue or refund after expiry",
            ));
        }
        let failure = state
            .settlement
            .fail(self.app_state.run_id(), "user_abandoned");
        self.publish(RuntimeEvent::Settlement(failure.event))
            .await?;
        let trade = runtime_trade_from_wallet_state(&state, SettlementStatus::Failed);
        self.persist_trade(&trade)?;
        self.trades.write().await.insert(trade_id, trade.clone());
        self.wallet_settlements.write().await.remove(&trade_id);
        self.delete_persisted_wallet_settlement(trade_id)?;
        Ok(trade)
    }

    /// Build an unsigned taker refund transaction for an expired locked settlement.
    ///
    /// # Errors
    ///
    /// Returns validation errors when the settlement is not refundable yet.
    pub async fn prepare_wallet_taker_refund(
        &self,
        trade_id: TradeId,
    ) -> AppResult<WalletTakerRefundPreparation> {
        let state = self.wallet_settlement_state(trade_id).await?;
        ensure_wallet_refundable(&state)?;
        if has_signature_kind(&state, TradeSignatureKind::TakerRefund) {
            return Err(AppError::validation("taker refund is already recorded"));
        }
        self.restore_wallet_settlement_htlc(&state).await?;
        let taker_refund_transaction = self
            .adapters
            .htlc_client
            .build_external_refund(trade_id, state.taker_wallet)
            .await?;
        Ok(WalletTakerRefundPreparation {
            trade_id,
            taker_refund_transaction,
        })
    }

    /// Record a browser taker refund and refund the maker leg when needed.
    ///
    /// # Errors
    ///
    /// Returns validation or adapter errors when refunding is not possible.
    pub async fn complete_wallet_taker_refund(
        &self,
        trade_id: TradeId,
        taker_refund_signature: TxSignature,
    ) -> AppResult<WalletTakerRefundResult> {
        if let Some(trade) = self.trades.read().await.get(&trade_id).cloned() {
            if trade.settlement_status == SettlementStatus::Refunded {
                return Ok(WalletTakerRefundResult {
                    maker_refund_signature: trade
                        .tx_signature_kinds
                        .iter()
                        .find(|signature| signature.kind == TradeSignatureKind::MakerRefund)
                        .map(|signature| TxSignature::new(signature.signature.clone())),
                    trade,
                });
            }
        }
        let mut state = self.wallet_settlement_state(trade_id).await?;
        ensure_wallet_refundable(&state)?;
        self.restore_wallet_settlement_htlc(&state).await?;

        if !has_signature_kind(&state, TradeSignatureKind::TakerRefund) {
            let taker_refund = self
                .adapters
                .htlc_client
                .record_external_refund(trade_id, taker_refund_signature)
                .await?;
            push_trade_signature(
                &mut state.tx_signatures,
                &mut state.tx_signature_kinds,
                &taker_refund,
            );
            let transition = state.settlement.record_refund(
                self.app_state.run_id(),
                SettlementLeg::TakerInput,
                taker_refund.signature.as_ref().map(ToString::to_string),
            );
            self.publish(RuntimeEvent::Settlement(transition.event))
                .await?;
            self.persist_and_store_wallet_settlement(&state).await?;
        }

        if has_signature_kind(&state, TradeSignatureKind::MakerLock)
            && !has_signature_kind(&state, TradeSignatureKind::MakerRefund)
            && !has_signature_kind(&state, TradeSignatureKind::MakerRedeem)
        {
            let maker_refund = self
                .adapters
                .htlc_client
                .refund_leg(trade_id, SettlementLeg::MakerOutput)
                .await?;
            push_trade_signature(
                &mut state.tx_signatures,
                &mut state.tx_signature_kinds,
                &maker_refund,
            );
            let transition = state.settlement.record_refund(
                self.app_state.run_id(),
                SettlementLeg::MakerOutput,
                maker_refund.signature.as_ref().map(ToString::to_string),
            );
            self.publish(RuntimeEvent::Settlement(transition.event))
                .await?;
            self.persist_and_store_wallet_settlement(&state).await?;
        }
        self.publish(RuntimeEvent::Settlement(SettlementEvent::StatusChanged {
            metadata: EventMetadata::new(self.app_state.run_id()),
            trade_id,
            status: SettlementStatus::Refunded,
        }))
        .await?;

        let trade = runtime_trade_from_wallet_state(&state, SettlementStatus::Refunded);
        self.persist_trade(&trade)?;
        self.trades.write().await.insert(trade_id, trade.clone());
        self.wallet_settlements.write().await.remove(&trade_id);
        self.delete_persisted_wallet_settlement(trade_id)?;

        Ok(WalletTakerRefundResult {
            trade,
            maker_refund_signature: signature_for_kind(&state, TradeSignatureKind::MakerRefund),
        })
    }

    async fn wallet_settlement_state(&self, trade_id: TradeId) -> AppResult<WalletSettlementState> {
        self.wallet_settlements
            .read()
            .await
            .get(&trade_id)
            .cloned()
            .ok_or_else(|| {
                AppError::validation(format!("unknown wallet settlement trade {trade_id}"))
            })
    }

    async fn fail_expired_prelock_wallet_settlement(
        &self,
        mut state: WalletSettlementState,
    ) -> AppResult<RuntimeTrade> {
        let trade_id = state.settlement.terms.trade_id;
        let failure = state
            .settlement
            .fail(self.app_state.run_id(), "quote_expired_before_lock");
        self.publish(RuntimeEvent::Settlement(failure.event))
            .await?;
        let trade = runtime_trade_from_wallet_state(&state, SettlementStatus::Failed);
        self.persist_trade(&trade)?;
        self.trades.write().await.insert(trade_id, trade.clone());
        self.wallet_settlements.write().await.remove(&trade_id);
        self.delete_persisted_wallet_settlement(trade_id)?;
        Ok(trade)
    }

    async fn restore_wallet_settlement_htlc(&self, state: &WalletSettlementState) -> AppResult<()> {
        let taker_locked = has_signature_kind(state, TradeSignatureKind::TakerLock);
        let maker_locked = has_signature_kind(state, TradeSignatureKind::MakerLock);
        if !taker_locked && !maker_locked {
            return Ok(());
        }
        self.adapters
            .htlc_client
            .restore_wallet_settlement(
                state.taker_lock_request.clone(),
                state.settlement.terms.maker_lock_request(),
                taker_locked,
                maker_locked,
            )
            .await
    }

    async fn run_settlement_locks(
        &self,
        settlement: &mut TwoSidedSettlement,
        quote: &FirmQuote,
        tx_signatures: &mut Vec<TxSignature>,
        tx_signature_kinds: &mut Vec<TradeSignature>,
    ) -> AppResult<()> {
        let start = settlement.start(self.app_state.run_id());
        self.publish(RuntimeEvent::Settlement(start.event)).await?;

        let taker_lock = match self
            .adapters
            .htlc_client
            .initiate(settlement.terms.taker_lock_request())
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                let reason = format!("taker HTLC initiate failed: {error}");
                self.record_settlement_failure(settlement, quote, tx_signatures, &reason)
                    .await?;
                return Err(AppError::solana(reason));
            }
        };
        push_trade_signature(tx_signatures, tx_signature_kinds, &taker_lock);
        let transition = settlement.record_taker_lock(
            self.app_state.run_id(),
            taker_lock.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await?;

        let status = match self
            .adapters
            .htlc_client
            .status(settlement.terms.trade_id)
            .await
        {
            Ok(status) => status,
            Err(error) => {
                let reason = format!("taker HTLC status poll failed: {error}");
                self.record_settlement_failure(settlement, quote, tx_signatures, &reason)
                    .await?;
                return Err(AppError::solana(reason));
            }
        };
        if status != SettlementStatus::Initiated {
            let reason = format!("taker HTLC validation returned {status:?}");
            self.record_settlement_failure(settlement, quote, tx_signatures, &reason)
                .await?;
            return Err(AppError::solana(reason));
        }
        let confirmation = settlement.confirm_taker_lock(self.app_state.run_id())?;
        self.publish(RuntimeEvent::Settlement(confirmation.event))
            .await?;

        // For Gateway-backed paths, run the real burn/mint adapter calls
        // (and when the output is non-USDC, the real Jupiter swap) before
        // the maker leg lands, so the consumer sees the full
        // gateway -> trading -> working_custody chain ahead of
        // `working_custody -> reserved`. Signatures are threaded into the
        // trade's signature collectors.
        self.run_gateway_backed_input_slice(
            quote,
            settlement.terms.trade_id,
            tx_signatures,
            tx_signature_kinds,
        )
        .await?;

        let maker_lock = match self
            .adapters
            .htlc_client
            .initiate(settlement.terms.maker_lock_request())
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                let reason = format!("maker HTLC initiate failed: {error}");
                self.record_settlement_failure(settlement, quote, tx_signatures, &reason)
                    .await?;
                return Err(AppError::solana(reason));
            }
        };
        push_trade_signature(tx_signatures, tx_signature_kinds, &maker_lock);
        let transition = settlement.record_maker_lock(
            self.app_state.run_id(),
            maker_lock.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await?;

        let status = match self
            .adapters
            .htlc_client
            .status(settlement.terms.trade_id)
            .await
        {
            Ok(status) => status,
            Err(error) => {
                let reason = format!("maker HTLC status poll failed: {error}");
                self.record_settlement_failure(settlement, quote, tx_signatures, &reason)
                    .await?;
                return Err(AppError::solana(reason));
            }
        };
        if status != SettlementStatus::Initiated {
            let reason = format!("maker HTLC validation returned {status:?}");
            self.record_settlement_failure(settlement, quote, tx_signatures, &reason)
                .await?;
            return Err(AppError::solana(reason));
        }
        let confirmation = settlement.confirm_maker_lock(self.app_state.run_id())?;
        self.publish(RuntimeEvent::Settlement(confirmation.event))
            .await
    }

    /// Drive the Gateway-backed-path on-chain effects between the confirmed
    /// taker lock and the maker HTLC submission, emitting the lifecycle
    /// events the ledger consumer needs as each adapter call lands:
    ///
    /// 1. `GatewayClient::request_refill` — burn USDC out of Gateway and
    ///    mint into `working_custody`. Emits
    ///    `Gateway::BurnIntentSubmitted` then `Gateway::MintConfirmed`. On
    ///    failure, emits `Gateway::BurnFailed` so the consumer reverses
    ///    `gateway_reserved → gateway`.
    /// 2. (non-USDC outputs only) `SwapExecutor::execute_swap` — swap
    ///    USDC → output asset on Jupiter. Emits `Swap::TradeSwapSubmitted`
    ///    then `Swap::TradeSwapConfirmed`. On failure post-submit, emits
    ///    `Swap::TradeSwapFailed` so the consumer reverses
    ///    `pending_dex_spend → working_custody`.
    ///
    /// Inventory-only paths skip the entire helper.
    async fn ensure_gateway_backed_input_slice(
        &self,
        state: &mut WalletSettlementState,
    ) -> AppResult<()> {
        if state.quote.execution_path != ExecutionPath::GatewayToDex {
            return Ok(());
        }

        let trade_id = state.settlement.terms.trade_id;
        let usdc = AssetId::from("USDC");
        let usdc_amount = if state.quote.output_amount.asset == usdc {
            state.quote.output_amount.clone()
        } else {
            TokenAmount::new(usdc.clone(), state.quote.input_amount.amount_raw)
        };

        if !has_signature_kind(state, TradeSignatureKind::GatewayMint) {
            self.run_gateway_burn_mint_slice(
                trade_id,
                &usdc_amount,
                &mut state.tx_signatures,
                &mut state.tx_signature_kinds,
            )
            .await?;
            self.persist_and_store_wallet_settlement(state).await?;
        }

        if state.quote.output_amount.asset != usdc
            && !has_signature_kind(state, TradeSignatureKind::JupiterSwap)
        {
            self.run_trade_jupiter_swap_slice(
                trade_id,
                &usdc,
                &usdc_amount,
                &state.quote.output_amount.asset,
                &mut state.tx_signatures,
                &mut state.tx_signature_kinds,
            )
            .await?;
            self.persist_and_store_wallet_settlement(state).await?;
        }

        Ok(())
    }

    async fn run_gateway_backed_input_slice(
        &self,
        quote: &FirmQuote,
        trade_id: TradeId,
        tx_signatures: &mut Vec<TxSignature>,
        tx_signature_kinds: &mut Vec<TradeSignature>,
    ) -> AppResult<()> {
        if quote.execution_path != ExecutionPath::GatewayToDex {
            return Ok(());
        }

        let usdc = AssetId::from("USDC");
        let usdc_amount = if quote.output_amount.asset == usdc {
            // USDC output: gateway delivers the full output amount.
            quote.output_amount.clone()
        } else {
            // Non-USDC output: the maker pulls USDC equivalent and swaps it
            // on Jupiter. Use `input_amount` (the taker's input) as a best
            // approximation of the USDC amount the maker needs to source —
            // fakes do not model swap economics so any monotone mapping
            // works; the real swap receipt's `output_amount` is what the
            // consumer trusts at confirm time.
            TokenAmount::new(usdc.clone(), quote.input_amount.amount_raw)
        };

        // Step A: Gateway burn → mint.
        self.run_gateway_burn_mint_slice(trade_id, &usdc_amount, tx_signatures, tx_signature_kinds)
            .await?;

        // Step B: Jupiter swap (non-USDC outputs only).
        if quote.output_amount.asset != usdc {
            self.run_trade_jupiter_swap_slice(
                trade_id,
                &usdc,
                &usdc_amount,
                &quote.output_amount.asset,
                tx_signatures,
                tx_signature_kinds,
            )
            .await?;
        }

        Ok(())
    }

    /// Drive `GatewayClient::request_refill` and emit
    /// `BurnIntentSubmitted` + `MintConfirmed` lifecycle events. On adapter
    /// failure (atomic — neither burn nor mint landed), surface
    /// `Gateway::Failed` for observers without moving the ledger.
    async fn run_gateway_burn_mint_slice(
        &self,
        trade_id: TradeId,
        usdc_amount: &TokenAmount,
        tx_signatures: &mut Vec<TxSignature>,
        tx_signature_kinds: &mut Vec<TradeSignature>,
    ) -> AppResult<()> {
        let refill_request = crate::domain::types::GatewayRefillRequest {
            amount: usdc_amount.clone(),
            destination: WalletRole::Maker,
        };
        let gateway_receipt = match self
            .adapters
            .gateway_client
            .request_refill(refill_request)
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                let reason = format!("gateway refill failed: {error}");
                self.publish(RuntimeEvent::Gateway(GatewayEvent::Failed {
                    metadata: EventMetadata::new(self.app_state.run_id()),
                    reason: reason.clone(),
                }))
                .await?;
                return Err(AppError::external_service("circle_gateway", reason));
            }
        };
        // Same-domain Solana Gateway transfer returns one composite
        // signature for the gatewayMint instruction. Surface it on both
        // lifecycle events so downstream consumers can correlate the trade
        // to the on-chain effect either way.
        let signature = gateway_receipt.signature;

        self.publish(RuntimeEvent::Gateway(GatewayEvent::BurnIntentSubmitted {
            metadata: EventMetadata::new(self.app_state.run_id()),
            trade_id,
            amount: usdc_amount.clone(),
            signature: signature.clone(),
        }))
        .await?;
        push_kinded_signature(
            tx_signatures,
            tx_signature_kinds,
            signature.as_ref(),
            TradeSignatureKind::GatewayBurn,
        );

        self.publish(RuntimeEvent::Gateway(GatewayEvent::MintConfirmed {
            metadata: EventMetadata::new(self.app_state.run_id()),
            trade_id,
            amount: usdc_amount.clone(),
            signature: signature.clone(),
        }))
        .await?;
        push_kinded_signature(
            tx_signatures,
            tx_signature_kinds,
            signature.as_ref(),
            TradeSignatureKind::GatewayMint,
        );
        Ok(())
    }

    /// Drive `SwapExecutor::execute_swap` for the trade-bound USDC → output
    /// swap and emit `TradeSwapSubmitted`/`TradeSwapConfirmed` lifecycle
    /// events. On execute failure, emit `TradeSwapFailed` so the consumer
    /// reverses `pending_dex_spend → working_custody`.
    async fn run_trade_jupiter_swap_slice(
        &self,
        trade_id: TradeId,
        usdc: &AssetId,
        usdc_amount: &TokenAmount,
        output_asset: &AssetId,
        tx_signatures: &mut Vec<TxSignature>,
        tx_signature_kinds: &mut Vec<TradeSignature>,
    ) -> AppResult<()> {
        let pair = crate::domain::types::AssetPair::new(usdc.clone(), output_asset.clone());
        let swap_request = crate::domain::types::SwapRequest {
            pair,
            input_amount: usdc_amount.clone(),
            source_wallet: WalletRole::Maker,
            destination_wallet: WalletRole::Maker,
            max_slippage_bps: self.app_state.config().jupiter.max_slippage_bps,
        };
        let swap_quote = self
            .adapters
            .swap_executor
            .quote_swap(swap_request)
            .await
            .map_err(|error| {
                AppError::solana(format!(
                    "jupiter quote failed for trade {trade_id}: {error}"
                ))
            })?;

        // Submit event must land before execute_swap so that any execute
        // failure has a `pending_dex_spend` reservation to unwind. The
        // submit event uses signature: None because the real on-chain
        // signature is reported by the adapter receipt and surfaced on the
        // confirmed event.
        self.publish(RuntimeEvent::Swap(SwapEvent::TradeSwapSubmitted {
            metadata: EventMetadata::new(self.app_state.run_id()),
            trade_id,
            input_amount: usdc_amount.clone(),
            signature: None,
        }))
        .await?;

        let swap_receipt = match self
            .adapters
            .swap_executor
            .execute_swap(swap_quote.clone())
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                let reason = format!("jupiter swap failed: {error}");
                self.publish(RuntimeEvent::Swap(SwapEvent::TradeSwapFailed {
                    metadata: EventMetadata::new(self.app_state.run_id()),
                    trade_id,
                    input_amount: usdc_amount.clone(),
                    reason: reason.clone(),
                }))
                .await?;
                return Err(AppError::solana(reason));
            }
        };

        let confirmed_output = swap_receipt
            .output_amount
            .clone()
            .unwrap_or_else(|| swap_quote.expected_output.clone());

        self.publish(RuntimeEvent::Swap(SwapEvent::TradeSwapConfirmed {
            metadata: EventMetadata::new(self.app_state.run_id()),
            trade_id,
            input_amount: usdc_amount.clone(),
            output_amount: confirmed_output,
            signature: Some(swap_receipt.signature.clone()),
        }))
        .await?;
        push_kinded_signature(
            tx_signatures,
            tx_signature_kinds,
            Some(&swap_receipt.signature),
            TradeSignatureKind::JupiterSwap,
        );
        Ok(())
    }

    async fn run_settlement_redeems(
        &self,
        settlement: &mut TwoSidedSettlement,
        quote: &FirmQuote,
        tx_signatures: &mut Vec<TxSignature>,
        tx_signature_kinds: &mut Vec<TradeSignature>,
        preimage: String,
    ) -> AppResult<()> {
        let trade_id = settlement.terms.trade_id;
        let taker_redeem = match self
            .adapters
            .htlc_client
            .redeem(trade_id, preimage.clone())
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                let reason = format!("taker redeem failed: {error}");
                self.record_settlement_failure(settlement, quote, tx_signatures, &reason)
                    .await?;
                return Err(AppError::solana(reason));
            }
        };
        push_trade_signature(tx_signatures, tx_signature_kinds, &taker_redeem);
        let transition = settlement.record_taker_redeem(
            self.app_state.run_id(),
            taker_redeem.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await?;

        let maker_redeem = match self.adapters.htlc_client.redeem(trade_id, preimage).await {
            Ok(receipt) => receipt,
            Err(error) => {
                let reason = format!("maker redeem failed: {error}");
                self.record_settlement_failure(settlement, quote, tx_signatures, &reason)
                    .await?;
                return Err(AppError::solana(reason));
            }
        };
        push_trade_signature(tx_signatures, tx_signature_kinds, &maker_redeem);
        let transition = settlement.record_maker_redeem(
            self.app_state.run_id(),
            maker_redeem.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await
    }

    /// Return a stored trade record.
    pub async fn trade(&self, trade_id: TradeId) -> Option<RuntimeTrade> {
        if let Some(trade) = self.trades.read().await.get(&trade_id).cloned() {
            return Some(trade);
        }
        self.wallet_settlements
            .read()
            .await
            .get(&trade_id)
            .map(|state| {
                runtime_trade_from_wallet_state(
                    state,
                    coarse_status_for_wallet_phase(state.settlement.phase),
                )
            })
    }

    /// Return up to `limit` of the most recently created trades,
    /// sorted newest-first by `created_at`.
    pub async fn recent_trades(&self, limit: usize) -> Vec<RuntimeTrade> {
        self.recent_trades_filtered(None, limit).await
    }

    /// Return up to `limit` of the most recently created trades for a taker
    /// wallet, sorted newest-first by `created_at`.
    pub async fn recent_trades_for_wallet(
        &self,
        wallet: &WalletAddress,
        limit: usize,
    ) -> Vec<RuntimeTrade> {
        self.recent_trades_filtered(Some(wallet), limit).await
    }

    async fn recent_trades_filtered(
        &self,
        wallet: Option<&WalletAddress>,
        limit: usize,
    ) -> Vec<RuntimeTrade> {
        let wallet = wallet.cloned();
        let mut trades = self.all_runtime_trades().await;
        if let Some(wallet) = wallet {
            trades.retain(|trade| trade.taker_wallet.as_ref() == Some(&wallet));
        }
        trades.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        trades.truncate(limit);
        trades
    }

    /// Return aggregate counts for terminal and active trades.
    pub async fn trade_counts(&self) -> RuntimeTradeCounts {
        self.trade_counts_filtered(None).await
    }

    /// Return aggregate counts scoped to one taker wallet.
    pub async fn trade_counts_for_wallet(&self, wallet: &WalletAddress) -> RuntimeTradeCounts {
        self.trade_counts_filtered(Some(wallet)).await
    }

    async fn trade_counts_filtered(&self, wallet: Option<&WalletAddress>) -> RuntimeTradeCounts {
        let wallet = wallet.cloned();
        let trades = self.trades.read().await;
        let wallet_settlements = self.wallet_settlements.read().await;
        let active_count = wallet_settlements
            .values()
            .filter(|state| {
                wallet
                    .as_ref()
                    .is_none_or(|wallet| &state.taker_wallet == wallet)
            })
            .count();
        let successful_count = trades
            .values()
            .filter(|trade| {
                wallet
                    .as_ref()
                    .is_none_or(|wallet| trade.taker_wallet.as_ref() == Some(wallet))
            })
            .filter(|trade| trade.settlement_status == SettlementStatus::Redeemed)
            .count();
        let refunded_count = trades
            .values()
            .filter(|trade| {
                wallet
                    .as_ref()
                    .is_none_or(|wallet| trade.taker_wallet.as_ref() == Some(wallet))
            })
            .filter(|trade| trade.settlement_status == SettlementStatus::Refunded)
            .count();
        let failed_count = trades
            .values()
            .filter(|trade| {
                wallet
                    .as_ref()
                    .is_none_or(|wallet| trade.taker_wallet.as_ref() == Some(wallet))
            })
            .filter(|trade| trade.settlement_status == SettlementStatus::Failed)
            .count();
        let terminal_count = trades
            .values()
            .filter(|trade| {
                wallet
                    .as_ref()
                    .is_none_or(|wallet| trade.taker_wallet.as_ref() == Some(wallet))
            })
            .count();
        RuntimeTradeCounts {
            total_count: terminal_count + active_count,
            successful_count,
            active_count,
            refunded_count,
            failed_count,
        }
    }

    async fn all_runtime_trades(&self) -> Vec<RuntimeTrade> {
        let mut trades: Vec<RuntimeTrade> = self.trades.read().await.values().cloned().collect();
        trades.extend(self.wallet_settlements.read().await.values().map(|state| {
            runtime_trade_from_wallet_state(
                state,
                coarse_status_for_wallet_phase(state.settlement.phase),
            )
        }));
        trades
    }

    /// Return the resolved [`ExecutionPath`] for a known trade. Wallet-flow
    /// trades not yet inserted into the trades map (still in
    /// `wallet_settlements`) are also covered.
    ///
    /// In-memory only — restarts wipe this. v0.2 follow-up adds durable trade
    /// persistence.
    pub async fn trade_path(&self, trade_id: TradeId) -> Option<ExecutionPath> {
        if let Some(trade) = self.trades.read().await.get(&trade_id) {
            return Some(trade.execution_path);
        }
        self.wallet_settlements
            .read()
            .await
            .get(&trade_id)
            .map(|state| state.quote.execution_path)
    }

    /// Return the latest durable ledger/P&L summary.
    ///
    /// # Errors
    ///
    /// Returns persistence errors when the durable summary cannot be read.
    pub fn ledger_summary(&self) -> AppResult<RuntimeLedgerSummary> {
        self.persistence.as_ref().map_or_else(
            || Ok(RuntimeLedgerSummary::default()),
            |persistence| persistence.summary(),
        )
    }

    /// Refresh working inventory and run rebalance and Gateway refill decisions
    /// from the resulting snapshot.
    ///
    /// Serialized via `automation_lock` so concurrent firings (the trade
    /// driven `accept_quote` path plus each always-on automation worker)
    /// cannot stack overlapping Jupiter/Gateway adapter actions or
    /// double-spend the cumulative cap.
    ///
    /// # Errors
    ///
    /// Returns balance-reader or projection errors. Individual automation
    /// adapter failures are emitted as runtime events and summarized.
    pub async fn refresh_inventory_and_automation(&self) -> AppResult<AutomationRunSummary> {
        let _guard = self.automation_lock.lock().await;
        self.refresh_inventory_and_automation_locked().await
    }

    async fn refresh_inventory_and_automation_locked(&self) -> AppResult<AutomationRunSummary> {
        let (inventory, inventory_policy) = self.refresh_automation_inventory().await?;
        self.run_automation(&inventory, &inventory_policy).await
    }

    async fn refresh_automation_inventory(
        &self,
    ) -> AppResult<(ValuedInventorySnapshot, InventoryPolicy)> {
        let balance_snapshot = self
            .adapters
            .balance_reader
            .balances(WalletRole::Maker)
            .await?;
        self.publish(RuntimeEvent::Inventory(InventoryEvent::Snapshot {
            metadata: EventMetadata::new(self.app_state.run_id()),
            snapshot: balance_snapshot.clone(),
        }))
        .await?;

        let inventory_policy = InventoryPolicy::from_config(self.app_state.config());
        let prices = self.usd_prices.read().await.clone();
        let inventory = ValuedInventorySnapshot::from_balance_snapshot(
            self.app_state.config(),
            balance_snapshot,
            &prices,
            &inventory_policy,
        )?;
        for event in inventory.inventory_events(
            self.app_state.run_id(),
            i32::from(self.app_state.config().risk.max_inventory_drift_bps),
        ) {
            self.publish(RuntimeEvent::Inventory(event)).await?;
        }

        Ok((inventory, inventory_policy))
    }

    /// Run one rebalance worker tick.
    ///
    /// # Errors
    ///
    /// Returns balance-reader or projection errors. Swap adapter failures are
    /// emitted as runtime events and summarized.
    pub async fn run_rebalance_check(&self) -> AppResult<AutomationRunSummary> {
        let _guard = self.automation_lock.lock().await;
        let (inventory, inventory_policy) = self.refresh_automation_inventory().await?;
        let mut summary = AutomationRunSummary {
            inventory_refreshed: true,
            ..AutomationRunSummary::default()
        };
        self.run_inventory_rebalance(&inventory, &inventory_policy, &mut summary)
            .await?;
        Ok(summary)
    }

    /// Run one Gateway refill worker tick.
    ///
    /// # Errors
    ///
    /// Returns balance-reader or projection errors. Gateway adapter failures
    /// are emitted as runtime events and summarized.
    pub async fn run_gateway_refill_check(&self) -> AppResult<AutomationRunSummary> {
        let _guard = self.automation_lock.lock().await;
        let (inventory, inventory_policy) = self.refresh_automation_inventory().await?;
        let mut summary = AutomationRunSummary {
            inventory_refreshed: true,
            ..AutomationRunSummary::default()
        };
        self.run_gateway_refill(&inventory, &inventory_policy, &mut summary)
            .await?;
        Ok(summary)
    }

    /// Run one Gateway excess deposit worker tick.
    ///
    /// # Errors
    ///
    /// Returns balance-reader or projection errors. Gateway adapter failures
    /// are emitted as runtime events and summarized.
    pub async fn run_excess_deposit_check(&self) -> AppResult<AutomationRunSummary> {
        let _guard = self.automation_lock.lock().await;
        let (inventory, inventory_policy) = self.refresh_automation_inventory().await?;
        let mut summary = AutomationRunSummary {
            inventory_refreshed: true,
            ..AutomationRunSummary::default()
        };
        self.run_excess_deposit(&inventory, &inventory_policy, &mut summary)
            .await?;
        Ok(summary)
    }

    /// Run one native SOL top-up worker tick.
    ///
    /// # Errors
    ///
    /// Returns balance-reader or projection errors. Swap adapter failures are
    /// emitted as runtime events and summarized.
    pub async fn run_native_top_up_check(&self) -> AppResult<AutomationRunSummary> {
        let _guard = self.automation_lock.lock().await;
        let (inventory, inventory_policy) = self.refresh_automation_inventory().await?;
        let mut summary = AutomationRunSummary {
            inventory_refreshed: true,
            ..AutomationRunSummary::default()
        };
        self.run_native_sol_top_up(&inventory, &inventory_policy, &mut summary)
            .await?;
        Ok(summary)
    }

    /// Publish a shutdown event. Future worker tasks can use this boundary to
    /// join cleanly when concrete workers are attached.
    ///
    /// # Errors
    ///
    /// Returns a projection publish error.
    pub async fn shutdown(&self, reason: impl Into<String>) -> AppResult<()> {
        self.publish(RuntimeEvent::System(SystemEvent::Shutdown {
            metadata: EventMetadata::new(self.app_state.run_id()),
            reason: reason.into(),
        }))
        .await
    }

    /// Generate the scripted normal tiny USDC->SOL RFQ used by legacy demos.
    ///
    /// # Errors
    ///
    /// Returns adapter, quote, risk, or event persistence errors from RFQ
    /// handling.
    pub async fn generate_tiny_demo_rfq(&self) -> AppResult<RfqResponse> {
        self.request_rfq(self.demo_rfq(AmountRaw::new(100_000))?)
            .await
    }

    /// Generate the scripted oversized RFQ used to demonstrate risk rejection.
    ///
    /// # Errors
    ///
    /// Returns adapter, quote, risk, or event persistence errors from RFQ
    /// handling.
    pub async fn generate_oversized_demo_rfq(&self) -> AppResult<RfqResponse> {
        self.request_rfq(self.demo_rfq(AmountRaw::new(1_000_000))?)
            .await
    }

    /// Generate a custom operator RFQ from configured asset identifiers.
    ///
    /// # Errors
    ///
    /// Returns configuration, validation, adapter, quote, risk, or event
    /// persistence errors from RFQ handling.
    pub async fn request_operator_rfq(
        &self,
        input_asset: &AssetId,
        output_asset: &AssetId,
        input_amount_raw: AmountRaw,
    ) -> AppResult<RfqResponse> {
        self.request_rfq(self.operator_rfq(input_asset, output_asset, input_amount_raw)?)
            .await
    }

    /// Accept the latest accepted quote generated through this orchestrator.
    ///
    /// # Errors
    ///
    /// Returns validation when no accepted quote exists, or settlement/adapter
    /// errors from the accept flow.
    pub async fn accept_latest_quote(&self) -> AppResult<RuntimeTrade> {
        let quote_id = (*self.last_accepted_quote.read().await)
            .ok_or_else(|| AppError::validation("no accepted quote is available to accept"))?;
        self.accept_quote(quote_id).await
    }

    /// Trigger an operator-requested post-settlement automation check.
    ///
    /// # Errors
    ///
    /// Returns balance-reader, adapter, or event persistence errors from the
    /// automation pass.
    pub async fn trigger_operator_automation_check(&self, label: &str) -> AppResult<()> {
        self.publish(RuntimeEvent::System(SystemEvent::HealthChanged {
            metadata: EventMetadata::new(self.app_state.run_id()),
            component: label.to_owned(),
            status: "requested".to_owned(),
        }))
        .await?;
        self.refresh_inventory_and_automation().await.map(|_| ())
    }

    async fn run_automation(
        &self,
        inventory: &ValuedInventorySnapshot,
        inventory_policy: &InventoryPolicy,
    ) -> AppResult<AutomationRunSummary> {
        let mut summary = AutomationRunSummary {
            inventory_refreshed: true,
            ..AutomationRunSummary::default()
        };

        self.run_native_sol_top_up(inventory, inventory_policy, &mut summary)
            .await?;
        if summary.completed_swaps == 0 {
            self.run_inventory_rebalance(inventory, inventory_policy, &mut summary)
                .await?;
        }
        self.run_gateway_refill(inventory, inventory_policy, &mut summary)
            .await?;

        Ok(summary)
    }

    async fn run_inventory_rebalance(
        &self,
        inventory: &ValuedInventorySnapshot,
        inventory_policy: &InventoryPolicy,
        summary: &mut AutomationRunSummary,
    ) -> AppResult<()> {
        let limits = AutomationLimits::from_config(self.app_state.config(), inventory_policy);
        let rebalance_policy =
            RebalancePolicy::from_config(self.app_state.config(), inventory_policy);
        let automation_state = self.automation_state.read().await.clone();
        match decide_inventory_rebalance(
            inventory,
            &automation_state,
            &limits,
            &rebalance_policy,
            OffsetDateTime::now_utc(),
        ) {
            RebalanceDecision::Swap(plan) => {
                if self.submit_swap_plan(plan, &limits).await? {
                    summary.completed_swaps += 1;
                }
            }
            RebalanceDecision::NoAction { reason } => {
                summary.blocked_reasons.push(reason);
                self.publish_cap_block_if_needed("rebalance", reason)
                    .await?;
            }
        }

        Ok(())
    }

    async fn run_native_sol_top_up(
        &self,
        inventory: &ValuedInventorySnapshot,
        inventory_policy: &InventoryPolicy,
        summary: &mut AutomationRunSummary,
    ) -> AppResult<()> {
        let limits = AutomationLimits::from_config(self.app_state.config(), inventory_policy);
        let rebalance_policy =
            RebalancePolicy::from_config(self.app_state.config(), inventory_policy);
        let automation_state = self.automation_state.read().await.clone();
        match decide_native_sol_top_up(
            inventory,
            &automation_state,
            &limits,
            &rebalance_policy,
            OffsetDateTime::now_utc(),
        ) {
            RebalanceDecision::Swap(plan) => {
                if !self
                    .ensure_native_top_up_usdc(&plan, inventory, &limits, summary)
                    .await?
                {
                    return Ok(());
                }
                if self.submit_swap_plan(plan, &limits).await? {
                    summary.completed_swaps += 1;
                }
            }
            RebalanceDecision::NoAction { reason } => {
                summary.blocked_reasons.push(reason);
                self.publish_cap_block_if_needed("native SOL top-up", reason)
                    .await?;
            }
        }

        Ok(())
    }

    async fn ensure_native_top_up_usdc(
        &self,
        plan: &PlannedSwap,
        inventory: &ValuedInventorySnapshot,
        limits: &AutomationLimits,
        summary: &mut AutomationRunSummary,
    ) -> AppResult<bool> {
        if plan.kind != AutomationActionKind::NativeSolTopUp || plan.pair.input.as_str() != "USDC" {
            return Ok(true);
        }

        let working_usdc = inventory.raw_balance(&plan.pair.input);
        if working_usdc.as_u64() >= plan.input_amount.amount_raw.as_u64() {
            return Ok(true);
        }

        let shortfall = AmountRaw::new(
            plan.input_amount
                .amount_raw
                .as_u64()
                .saturating_sub(working_usdc.as_u64()),
        );
        if shortfall.is_zero() {
            return Ok(true);
        }

        let refill_plan = GatewayRefillPlan {
            request: GatewayRefillRequest {
                amount: TokenAmount::new(plan.pair.input.clone(), shortfall),
                destination: WalletRole::Maker,
            },
            estimated_notional_usd: raw_to_units(shortfall, 6),
        };

        if self.submit_gateway_plan(refill_plan, limits).await? {
            summary.completed_gateway_refills += 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn run_gateway_refill(
        &self,
        inventory: &ValuedInventorySnapshot,
        inventory_policy: &InventoryPolicy,
        summary: &mut AutomationRunSummary,
    ) -> AppResult<()> {
        let limits = AutomationLimits::from_config(self.app_state.config(), inventory_policy);
        let automation_state = self.automation_state.read().await.clone();
        match decide_gateway_refill(
            inventory,
            &automation_state,
            &limits,
            self.app_state.config(),
            OffsetDateTime::now_utc(),
        ) {
            GatewayRefillDecision::Refill(plan) => {
                if self.submit_gateway_plan(plan, &limits).await? {
                    summary.completed_gateway_refills += 1;
                }
            }
            GatewayRefillDecision::NoAction { reason } => {
                summary.blocked_reasons.push(reason);
                if reason == DecisionBlockReason::CumulativeCapExceeded {
                    self.publish(RuntimeEvent::Gateway(GatewayEvent::Failed {
                        metadata: EventMetadata::new(self.app_state.run_id()),
                        reason: "gateway refill blocked by cumulative cap".to_owned(),
                    }))
                    .await?;
                }
            }
        }

        Ok(())
    }

    async fn run_excess_deposit(
        &self,
        inventory: &ValuedInventorySnapshot,
        _inventory_policy: &InventoryPolicy,
        summary: &mut AutomationRunSummary,
    ) -> AppResult<()> {
        let Some(plan) = decide_excess_deposit(inventory, &self.app_state.config().gateway) else {
            summary.blocked_reasons.push(DecisionBlockReason::NoDrift);
            return Ok(());
        };

        if self.submit_gateway_deposit(plan).await? {
            summary.completed_gateway_deposits += 1;
        }

        Ok(())
    }

    async fn submit_swap_plan(
        &self,
        plan: PlannedSwap,
        limits: &AutomationLimits,
    ) -> AppResult<bool> {
        let key = plan.action_key();
        self.mark_action_in_flight(key.clone()).await;
        let submission = submit_planned_swap(
            self.adapters.swap_executor.as_ref(),
            &plan,
            limits,
            self.app_state.run_id(),
        )
        .await;
        self.clear_action_in_flight(&key).await;

        match submission {
            Ok(submission) => {
                self.record_completed_action(
                    &key,
                    plan.estimated_notional_usd,
                    plan.uses_non_stable_asset_exception,
                )
                .await;
                for event in submission.events {
                    self.publish(RuntimeEvent::Swap(event)).await?;
                }
                Ok(true)
            }
            Err(error) => {
                self.publish(RuntimeEvent::Swap(SwapEvent::Failed {
                    metadata: EventMetadata::new(self.app_state.run_id()),
                    pair: Some(plan.pair),
                    reason: error.to_string(),
                }))
                .await?;
                Ok(false)
            }
        }
    }

    async fn submit_gateway_plan(
        &self,
        plan: GatewayRefillPlan,
        _limits: &AutomationLimits,
    ) -> AppResult<bool> {
        let usdc = plan.request.amount.asset.clone();
        let key = ActionKey {
            kind: AutomationActionKind::GatewayRefill,
            source_asset: usdc.clone(),
            dest_asset: usdc,
        };
        self.mark_action_in_flight(key.clone()).await;
        self.publish(RuntimeEvent::Gateway(GatewayEvent::RefillRequested {
            metadata: EventMetadata::new(self.app_state.run_id()),
            amount: plan.request.amount.clone(),
        }))
        .await?;
        let receipt = submit_gateway_refill(self.adapters.gateway_client.as_ref(), &plan).await;
        self.clear_action_in_flight(&key).await;

        match receipt {
            Ok(receipt) => {
                self.record_completed_action(&key, plan.estimated_notional_usd, false)
                    .await;
                self.publish(RuntimeEvent::Gateway(GatewayEvent::RefillCompleted {
                    metadata: EventMetadata::new(self.app_state.run_id()),
                    receipt,
                }))
                .await?;
                Ok(true)
            }
            Err(error) => {
                self.publish(RuntimeEvent::Gateway(GatewayEvent::RefillFailed {
                    metadata: EventMetadata::new(self.app_state.run_id()),
                    amount: plan.request.amount,
                    reason: error.to_string(),
                }))
                .await?;
                Ok(false)
            }
        }
    }

    async fn submit_gateway_deposit(&self, plan: ExcessDepositPlan) -> AppResult<bool> {
        let usdc = AssetId::from("USDC");
        let amount = TokenAmount::new(usdc.clone(), plan.amount_raw);
        let key = ActionKey {
            kind: AutomationActionKind::GatewayDeposit,
            source_asset: usdc.clone(),
            dest_asset: usdc,
        };
        self.mark_action_in_flight(key.clone()).await;
        self.publish(RuntimeEvent::Gateway(GatewayEvent::DepositSubmitted {
            metadata: EventMetadata::new(self.app_state.run_id()),
            amount: amount.clone(),
            signature: None,
        }))
        .await?;

        let receipt = self.adapters.gateway_client.deposit(plan.amount_raw).await;
        self.clear_action_in_flight(&key).await;

        match receipt {
            Ok(receipt) => {
                self.record_completed_action(&key, Decimal::ZERO, false)
                    .await;
                self.publish(RuntimeEvent::Gateway(GatewayEvent::DepositConfirmed {
                    metadata: EventMetadata::new(self.app_state.run_id()),
                    receipt,
                }))
                .await?;
                Ok(true)
            }
            Err(error) => {
                self.publish(RuntimeEvent::Gateway(GatewayEvent::DepositFailed {
                    metadata: EventMetadata::new(self.app_state.run_id()),
                    amount,
                    signature: None,
                    reason: error.to_string(),
                }))
                .await?;
                Ok(false)
            }
        }
    }

    async fn publish_cap_block_if_needed(
        &self,
        action: &str,
        reason: DecisionBlockReason,
    ) -> AppResult<()> {
        if reason == DecisionBlockReason::CumulativeCapExceeded {
            self.publish(RuntimeEvent::Swap(SwapEvent::Failed {
                metadata: EventMetadata::new(self.app_state.run_id()),
                pair: None,
                reason: format!("{action} blocked by cumulative cap"),
            }))
            .await?;
        }
        Ok(())
    }

    async fn mark_action_in_flight(&self, key: ActionKey) {
        self.automation_state.write().await.in_flight.push(key);
    }

    async fn clear_action_in_flight(&self, key: &ActionKey) {
        self.automation_state
            .write()
            .await
            .in_flight
            .retain(|in_flight| in_flight != key);
    }

    async fn record_completed_action(
        &self,
        key: &ActionKey,
        notional_usd: Decimal,
        uses_non_stable_asset_exception: bool,
    ) {
        let mut state = self.automation_state.write().await;
        state.cumulative_spend_usd += notional_usd;
        state
            .last_submitted_at
            .insert(key.clone(), OffsetDateTime::now_utc());
        if uses_non_stable_asset_exception {
            state.non_stable_asset_exception_used = true;
        }
    }

    async fn record_settlement_failure(
        &self,
        settlement: &mut TwoSidedSettlement,
        quote: &FirmQuote,
        tx_signatures: &[TxSignature],
        reason: &str,
    ) -> AppResult<()> {
        let failure = settlement.fail(self.app_state.run_id(), reason.to_owned());
        self.publish(RuntimeEvent::Settlement(failure.event))
            .await?;
        let trade = RuntimeTrade {
            quote_id: quote.quote_id,
            trade_id: settlement.terms.trade_id,
            run_id: self.app_state.run_id(),
            taker_wallet: Some(quote.htlc_terms.taker_wallet.clone()),
            expires_at: Some(settlement.terms.expires_at),
            settlement_status: SettlementStatus::Failed,
            tx_signatures: tx_signatures.to_vec(),
            tx_signature_kinds: Vec::new(),
            created_at: OffsetDateTime::now_utc(),
            input_amount: quote.input_amount.clone(),
            output_amount: quote.output_amount.clone(),
            execution_path: quote.execution_path,
        };
        self.persist_trade(&trade)?;
        self.trades
            .write()
            .await
            .insert(settlement.terms.trade_id, trade);
        Ok(())
    }

    async fn record_usd_price(&self, price: &crate::domain::types::ReferencePrice) {
        let mut prices = self.usd_prices.write().await;
        prices.insert(AssetId::from("USDC"), Decimal::ONE);

        if price.pair.output.as_str() == "USDC" {
            prices.insert(price.pair.input.clone(), price.output_per_input);
        } else if price.pair.input.as_str() == "USDC" && price.output_per_input > Decimal::ZERO {
            if let Some(inverse) = Decimal::ONE.checked_div(price.output_per_input) {
                prices.insert(price.pair.output.clone(), inverse);
            }
        }
    }

    /// Publish a runtime event through the standard projection + persistence
    /// pipeline. Used by always-on workers that need to emit events without
    /// going through a settlement or RFQ flow.
    ///
    /// # Errors
    ///
    /// Returns projection or persistence errors when the event cannot be
    /// published.
    pub async fn publish_runtime_event(&self, event: RuntimeEvent) -> AppResult<()> {
        self.publish(event).await
    }

    async fn publish(&self, event: RuntimeEvent) -> AppResult<()> {
        self.app_state
            .runtime()
            .publish_event(event.clone())
            .await?;
        if let Some(persistence) = &self.persistence {
            persistence.consume(&event)?;
            self.app_state
                .runtime()
                .update_pnl_projection(persistence.pnl_projection()?)
                .await;
        }
        Ok(())
    }

    fn demo_rfq(&self, amount_raw: AmountRaw) -> AppResult<RfqRequest> {
        self.operator_rfq(&AssetId::from("USDC"), &AssetId::from("SOL"), amount_raw)
    }

    fn operator_rfq(
        &self,
        input_asset: &AssetId,
        output_asset: &AssetId,
        amount_raw: AmountRaw,
    ) -> AppResult<RfqRequest> {
        if input_asset == output_asset {
            return Err(AppError::validation(
                "RFQ input and output assets must be different",
            ));
        }

        let input_mint = self.mint_for_asset(input_asset)?;
        let output_mint = self.mint_for_asset(output_asset)?;
        let taker_wallet = self.demo_taker_wallet.clone().ok_or_else(|| {
            AppError::config("legacy local RFQ commands require a configured taker wallet address")
        })?;

        Ok(RfqRequest {
            input_mint,
            output_mint,
            input_amount_raw: amount_raw,
            taker_wallet,
            expiry_seconds: Some(
                self.app_state
                    .config()
                    .assets
                    .policy
                    .default_quote_expiry_seconds,
            ),
        })
    }

    fn mint_for_asset(&self, id: &AssetId) -> AppResult<crate::domain::types::MintAddress> {
        self.app_state
            .config()
            .assets
            .supported
            .iter()
            .find(|asset| asset.enabled && asset.id == *id)
            .map(|asset| asset.mint.clone())
            .ok_or_else(|| AppError::config(format!("missing {id} asset config")))
    }
}

fn quote_inventory_from_balance(
    _config: &AppConfig,
    balance_snapshot: &BalanceSnapshot,
) -> QuoteInventorySnapshot {
    QuoteInventorySnapshot {
        balances: balance_snapshot.balances.clone(),
        targets: balance_snapshot.balances.clone(),
        observed_at: balance_snapshot.observed_at,
    }
}

/// Convert a raw USDC amount into the equivalent raw amount of `output_asset`
/// using the supplied USDC -> output `output_per_input` price. Returns zero
/// when any of the conversions overflows or the price is non-positive — the
/// caller treats the synthetic supply as absent in that case.
fn convert_usdc_to_asset_raw(
    usdc_raw: AmountRaw,
    usdc_decimals: u8,
    usdc_to_output_price: Decimal,
    output_decimals: u8,
) -> u64 {
    use rust_decimal::prelude::ToPrimitive;

    if usdc_to_output_price <= Decimal::ZERO {
        return 0;
    }
    let mut usdc_factor = Decimal::ONE;
    for _ in 0..usdc_decimals {
        usdc_factor *= Decimal::from(10);
    }
    let mut output_factor = Decimal::ONE;
    for _ in 0..output_decimals {
        output_factor *= Decimal::from(10);
    }
    let usdc_ui = Decimal::from(usdc_raw.as_u64()) / usdc_factor;
    let output_ui = usdc_ui * usdc_to_output_price;
    let scaled = (output_ui * output_factor).trunc();
    scaled.to_u64().unwrap_or(0)
}

impl RuntimeOrchestrator {
    /// Treat FREE Gateway USDC as additional supply for the requested output
    /// asset. T9 scope: this only widens the gate the inventory-first risk
    /// rule sees — it does not yet formalize an `ExecutionPath` enum or wire a
    /// distinct settlement path (that is worktree C, T-C1).
    ///
    /// Behaviour:
    /// * No persistence wired -> nothing to do.
    /// * Output asset is USDC -> add free Gateway USDC directly.
    /// * Output asset is non-USDC -> fetch the `(USDC, output)` reference
    ///   price and convert free Gateway USDC into output-asset units.
    /// * Free Gateway USDC is zero or the asset/mint is unknown -> nothing
    ///   to add.
    async fn augment_inventory_with_gateway_supply(
        &self,
        request: &RfqRequest,
        inventory: &mut QuoteInventorySnapshot,
    ) -> AppResult<()> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(());
        };

        let usdc = AssetId::from("USDC");
        let free_gateway_usdc = persistence.gateway_free_balance(&usdc)?;
        if free_gateway_usdc.is_zero() {
            return Ok(());
        }

        let Some(output_asset) = self
            .app_state
            .config()
            .assets
            .supported
            .iter()
            .find(|asset| asset.enabled && asset.mint == request.output_mint)
            .cloned()
        else {
            // Unknown output mint — request_quote will reject with
            // UnsupportedAsset before reaching the inventory gate.
            return Ok(());
        };

        let synthetic_output_raw = if output_asset.id == usdc {
            free_gateway_usdc.as_u64()
        } else {
            let usdc_decimals = self
                .app_state
                .config()
                .assets
                .supported
                .iter()
                .find(|asset| asset.enabled && asset.id == usdc)
                .map_or(6, |asset| asset.decimals);
            let pair = crate::domain::types::AssetPair::new(usdc.clone(), output_asset.id.clone());
            let Ok(reference_price) = self.adapters.price_provider.reference_price(pair).await
            else {
                // No price route from USDC to the output asset. The Gateway
                // path requires a feasible Jupiter route in the live runtime;
                // when the price feed cannot value it, surface no synthetic
                // supply and let the inventory-first gate decide.
                return Ok(());
            };
            convert_usdc_to_asset_raw(
                free_gateway_usdc,
                usdc_decimals,
                reference_price.output_per_input,
                output_asset.decimals,
            )
        };

        if synthetic_output_raw == 0 {
            return Ok(());
        }

        // Fold the synthetic supply into the existing balance entry, or push a
        // new one if the asset isn't in the snapshot yet.
        if let Some(entry) = inventory
            .balances
            .iter_mut()
            .find(|amount| amount.asset == output_asset.id)
        {
            let combined = entry
                .amount_raw
                .as_u64()
                .saturating_add(synthetic_output_raw);
            entry.amount_raw = AmountRaw::new(combined);
        } else {
            inventory.balances.push(TokenAmount::new(
                output_asset.id.clone(),
                AmountRaw::new(synthetic_output_raw),
            ));
        }

        Ok(())
    }

    /// Build an RFQ inventory snapshot from the ledger's `working_custody`
    /// balances when durable persistence is wired. Returns `None` when no
    /// persistence is configured so callers can fall back to the live wallet
    /// snapshot.
    fn ledger_quote_inventory(
        &self,
        balance_snapshot: &BalanceSnapshot,
    ) -> AppResult<Option<QuoteInventorySnapshot>> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(None);
        };

        let mut balances = Vec::with_capacity(self.app_state.config().assets.supported.len());
        for asset in &self.app_state.config().assets.supported {
            if !asset.enabled {
                continue;
            }
            let amount = persistence.working_custody_balance(&asset.id)?;
            balances.push(TokenAmount::new(asset.id.clone(), amount));
        }

        Ok(Some(QuoteInventorySnapshot {
            balances: balances.clone(),
            targets: balances,
            observed_at: balance_snapshot.observed_at,
        }))
    }
}

/// Append a signature plus its kind discriminator derived from the receipt's
/// `(leg, status)` pair. No-op when the receipt has no signature or the
/// kind is not a trade-summary state.
fn push_trade_signature(
    plain: &mut Vec<TxSignature>,
    kinded: &mut Vec<TradeSignature>,
    receipt: &crate::domain::types::HtlcReceipt,
) {
    if let Some(signature) = receipt.signature.as_ref() {
        if !plain.iter().any(|existing| existing == signature) {
            plain.push(signature.clone());
        }
    }
    if let (Some(signature), Some(kind)) = (
        receipt.signature.as_ref(),
        TradeSignatureKind::from_htlc_receipt(receipt.leg, receipt.status),
    ) {
        if !kinded
            .iter()
            .any(|existing| existing.kind == kind && existing.signature == signature.as_str())
        {
            kinded.push(TradeSignature {
                kind,
                signature: signature.as_str().to_owned(),
            });
        }
    }
}

/// Append a Gateway/Jupiter signature with the supplied kind. No-op when no
/// signature is present (real Gateway transfers may complete without a
/// trade-bound signature when the adapter is configured without a mint
/// submitter, in which case the lifecycle event still fires for the ledger).
fn push_kinded_signature(
    plain: &mut Vec<TxSignature>,
    kinded: &mut Vec<TradeSignature>,
    signature: Option<&TxSignature>,
    kind: TradeSignatureKind,
) {
    if let Some(signature) = signature {
        if !plain.iter().any(|existing| existing == signature) {
            plain.push(signature.clone());
        }
        if !kinded
            .iter()
            .any(|existing| existing.kind == kind && existing.signature == signature.as_str())
        {
            kinded.push(TradeSignature {
                kind,
                signature: signature.as_str().to_owned(),
            });
        }
    }
}
