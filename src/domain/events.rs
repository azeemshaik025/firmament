//! Runtime event contracts consumed by API, web app, and ledger projections.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::types::{
    AssetId, AssetPair, BalanceSnapshot, GatewayReceipt, HtlcReceipt, QuoteId, ReferencePrice,
    RejectionReason, RiskDecision, RuntimeRunId, SettlementStatus, SwapQuote, SwapReceipt,
    TokenAmount, TradeId, TxSignature, WalletAddress,
};

/// Shared event metadata for ordering and runtime correlation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventMetadata {
    /// Unique event identifier.
    pub event_id: Uuid,
    /// Runtime invocation that produced the event.
    pub run_id: RuntimeRunId,
    /// UTC timestamp for the event.
    pub occurred_at: OffsetDateTime,
}

impl EventMetadata {
    /// Create metadata for a new runtime event.
    #[must_use]
    pub fn new(run_id: RuntimeRunId) -> Self {
        Self {
            event_id: Uuid::now_v7(),
            run_id,
            occurred_at: OffsetDateTime::now_utc(),
        }
    }
}

/// Top-level runtime event category.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "category", content = "event", rename_all = "snake_case")]
pub enum RuntimeEvent {
    /// Quote lifecycle event.
    Quote(QuoteEvent),
    /// HTLC settlement event.
    Settlement(SettlementEvent),
    /// Jupiter swap or rebalance event.
    Swap(SwapEvent),
    /// Circle Gateway event.
    Gateway(GatewayEvent),
    /// Risk decision event.
    Risk(RiskEvent),
    /// Inventory projection event.
    Inventory(InventoryEvent),
    /// Reconciliation observation tick (always emitted, even on skip).
    Reconciliation(ReconciliationEvent),
    /// Always-on automation worker tick.
    Automation(AutomationEvent),
    /// Runtime system event.
    System(SystemEvent),
}

/// Quote lifecycle events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuoteEvent {
    /// RFQ was received and assigned an identifier.
    Requested {
        /// Event metadata.
        metadata: EventMetadata,
        /// Quote identifier.
        quote_id: QuoteId,
        /// Requested pair.
        pair: AssetPair,
        /// Requested input amount.
        input_amount: TokenAmount,
        /// Taker wallet address supplied by the caller.
        taker_wallet: WalletAddress,
        /// Quote expiry timestamp.
        expires_at: OffsetDateTime,
    },
    /// Quote was accepted and mapped to a trade.
    Accepted {
        /// Event metadata.
        metadata: EventMetadata,
        /// Quote identifier.
        quote_id: QuoteId,
        /// Trade identifier.
        trade_id: TradeId,
    },
    /// Quote was rejected by validation or risk.
    Rejected {
        /// Event metadata.
        metadata: EventMetadata,
        /// Quote identifier, if assigned before rejection.
        quote_id: Option<QuoteId>,
        /// Stable rejection reason.
        reason: RejectionReason,
        /// Full decision details.
        decision: RiskDecision,
    },
    /// Quote expired without acceptance.
    Expired {
        /// Event metadata.
        metadata: EventMetadata,
        /// Quote identifier.
        quote_id: QuoteId,
    },
}

/// HTLC settlement events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettlementEvent {
    /// Settlement flow started.
    Started {
        /// Event metadata.
        metadata: EventMetadata,
        /// Trade identifier.
        trade_id: TradeId,
        /// Quote identifier that produced the trade.
        quote_id: QuoteId,
    },
    /// HTLC transaction was submitted on chain (not yet confirmed).
    ///
    /// Emitted immediately after the transaction lands in the mempool / RPC,
    /// before any confirmation signal is observed. Replaces the pre-T5b
    /// `Initiated` variant in conjunction with [`SettlementEvent::Confirmed`];
    /// downstream consumers that previously matched on `"type": "initiated"`
    /// must now handle `"submitted"` and `"confirmed"` independently.
    Submitted {
        /// Event metadata.
        metadata: EventMetadata,
        /// HTLC receipt with leg, amount, and pending status/signature.
        receipt: HtlcReceipt,
    },
    /// HTLC transaction was confirmed on chain.
    ///
    /// Emitted after the on-chain HTLC account is observed (status poll
    /// reports `Initiated` against the live program state), signalling that
    /// the lock has reached at least the configured commitment level.
    Confirmed {
        /// Event metadata.
        metadata: EventMetadata,
        /// HTLC receipt with confirmed status.
        receipt: HtlcReceipt,
    },
    /// HTLC was redeemed.
    Redeemed {
        /// Event metadata.
        metadata: EventMetadata,
        /// HTLC receipt.
        receipt: HtlcReceipt,
    },
    /// HTLC was refunded.
    Refunded {
        /// Event metadata.
        metadata: EventMetadata,
        /// HTLC receipt.
        receipt: HtlcReceipt,
    },
    /// Settlement status changed without a receipt update.
    StatusChanged {
        /// Event metadata.
        metadata: EventMetadata,
        /// Trade identifier.
        trade_id: TradeId,
        /// New status.
        status: SettlementStatus,
    },
    /// Settlement failed before completion.
    Failed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Trade identifier.
        trade_id: TradeId,
        /// Operator-facing reason.
        reason: String,
    },
}

/// Jupiter swap and rebalance events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SwapEvent {
    /// Reference price was observed.
    PriceObserved {
        /// Event metadata.
        metadata: EventMetadata,
        /// Price snapshot.
        price: ReferencePrice,
    },
    /// Swap quote was received.
    Quoted {
        /// Event metadata.
        metadata: EventMetadata,
        /// Swap quote.
        quote: SwapQuote,
    },
    /// Swap transaction was submitted or confirmed.
    Executed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Swap receipt.
        receipt: SwapReceipt,
    },
    /// Trade-correlated Jupiter swap input was submitted on chain. Carries the
    /// debited input amount so the ledger can move
    /// `working_custody → pending_dex_spend` for the trade.
    ///
    /// Background rebalance swaps do not emit this variant — they continue to
    /// follow [`SwapEvent::Executed`] with `receipt.trade_id = None`.
    TradeSwapSubmitted {
        /// Event metadata.
        metadata: EventMetadata,
        /// Trade this swap belongs to.
        trade_id: TradeId,
        /// Asset/amount debited from working custody.
        input_amount: TokenAmount,
        /// On-chain Jupiter swap signature, when the adapter returned one.
        signature: Option<TxSignature>,
    },
    /// Trade-correlated Jupiter swap was confirmed on chain. Carries both
    /// sides so the ledger can move `pending_dex_spend → trading` (input)
    /// AND `trading → working_custody` (output) atomically.
    TradeSwapConfirmed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Trade this swap belongs to.
        trade_id: TradeId,
        /// Asset/amount that left working custody.
        input_amount: TokenAmount,
        /// Asset/amount that arrived in working custody.
        output_amount: TokenAmount,
        /// On-chain Jupiter swap signature, when the adapter returned one.
        signature: Option<TxSignature>,
    },
    /// Trade-correlated Jupiter swap failed after it had been submitted —
    /// unwind `pending_dex_spend → working_custody`.
    TradeSwapFailed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Trade this swap belongs to.
        trade_id: TradeId,
        /// Asset/amount previously committed via [`SwapEvent::TradeSwapSubmitted`].
        input_amount: TokenAmount,
        /// Operator-facing reason.
        reason: String,
    },
    /// Swap failed before completion.
    Failed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Related pair, if known.
        pair: Option<AssetPair>,
        /// Operator-facing reason.
        reason: String,
    },
}

/// Circle Gateway events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayEvent {
    /// Gateway balance was checked.
    BalanceChecked {
        /// Event metadata.
        metadata: EventMetadata,
        /// Balance snapshot.
        balance: TokenAmount,
    },
    /// Gateway refill was requested.
    RefillRequested {
        /// Event metadata.
        metadata: EventMetadata,
        /// Requested amount.
        amount: TokenAmount,
    },
    /// Gateway refill completed or was submitted.
    RefillCompleted {
        /// Event metadata.
        metadata: EventMetadata,
        /// Gateway receipt.
        receipt: GatewayReceipt,
    },
    /// Gateway refill failed after the runtime opened pending state.
    RefillFailed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Amount to return from pending Gateway deposit to Gateway.
        amount: TokenAmount,
        /// Operator-facing reason.
        reason: String,
    },
    /// Excess working USDC deposit was submitted to Solana Gateway.
    DepositSubmitted {
        /// Event metadata.
        metadata: EventMetadata,
        /// Amount moved out of working custody.
        amount: TokenAmount,
        /// On-chain Gateway deposit signature, when known at submit time.
        signature: Option<TxSignature>,
    },
    /// Excess working USDC deposit is reflected in Gateway.
    DepositConfirmed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Gateway receipt.
        receipt: GatewayReceipt,
    },
    /// Excess working USDC deposit failed after the runtime opened pending state.
    DepositFailed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Amount to return from pending Gateway deposit to working custody.
        amount: TokenAmount,
        /// On-chain Gateway deposit signature, when known.
        signature: Option<TxSignature>,
        /// Operator-facing reason.
        reason: String,
    },
    /// Trade-correlated Gateway burn intent submitted. Drives a compound
    /// ledger movement `gateway → gateway_reserved` (reservation) followed
    /// immediately by `gateway_reserved → trading` (burn) — same back-to-back
    /// pattern the maker leg uses for `Submitted{MakerOutput}`.
    ///
    /// Only emitted on the Gateway-backed execution path.
    BurnIntentSubmitted {
        /// Event metadata.
        metadata: EventMetadata,
        /// Trade this burn belongs to.
        trade_id: TradeId,
        /// USDC amount being burned for the trade.
        amount: TokenAmount,
        /// On-chain Gateway burn signature, when the adapter returned one.
        signature: Option<TxSignature>,
    },
    /// Trade-correlated Gateway mint confirmed. Moves `trading → working_custody`
    /// for the USDC asset.
    MintConfirmed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Trade this mint belongs to.
        trade_id: TradeId,
        /// USDC amount minted for the trade.
        amount: TokenAmount,
        /// On-chain Gateway mint signature, when the adapter returned one.
        signature: Option<TxSignature>,
    },
    /// Trade-correlated Gateway burn failed after the reservation landed —
    /// release `gateway_reserved → gateway`.
    BurnFailed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Trade this burn belonged to.
        trade_id: TradeId,
        /// USDC amount previously reserved.
        amount: TokenAmount,
        /// Operator-facing reason.
        reason: String,
    },
    /// Gateway operation failed.
    Failed {
        /// Event metadata.
        metadata: EventMetadata,
        /// Operator-facing reason.
        reason: String,
    },
}

/// Risk evaluation events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RiskEvent {
    /// Risk decision was produced for a quote or action.
    Evaluated {
        /// Event metadata.
        metadata: EventMetadata,
        /// Quote identifier, when the decision belongs to an RFQ.
        quote_id: Option<QuoteId>,
        /// Risk decision.
        decision: RiskDecision,
    },
    /// A risk limit changed through config or operator action.
    LimitUpdated {
        /// Event metadata.
        metadata: EventMetadata,
        /// Stable risk limit label.
        limit: String,
        /// Redacted or non-secret value shown to the operator.
        value: String,
    },
}

/// Inventory projection events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InventoryEvent {
    /// Wallet balances were observed.
    Snapshot {
        /// Event metadata.
        metadata: EventMetadata,
        /// Balance snapshot.
        snapshot: BalanceSnapshot,
    },
    /// Inventory drift exceeded a configured threshold.
    DriftDetected {
        /// Event metadata.
        metadata: EventMetadata,
        /// Asset with drift.
        asset: TokenAmount,
        /// Drift in basis points.
        drift_bps: i32,
    },
    /// An asset crossed its quoteable threshold.
    ThresholdBreached {
        /// Event metadata.
        metadata: EventMetadata,
        /// Asset amount at breach time.
        asset: TokenAmount,
        /// Stable rejection reason tied to the breach.
        reason: RejectionReason,
    },
}

/// Runtime system events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemEvent {
    /// Runtime has loaded config and built its initial state.
    #[serde(alias = "scaffold_ready")]
    RuntimeReady {
        /// Event metadata.
        metadata: EventMetadata,
        /// Human-readable startup note.
        message: String,
    },
    /// Runtime health changed.
    HealthChanged {
        /// Event metadata.
        metadata: EventMetadata,
        /// Component reporting health.
        component: String,
        /// Health status label.
        status: String,
    },
    /// Runtime shutdown was requested or completed.
    Shutdown {
        /// Event metadata.
        metadata: EventMetadata,
        /// Shutdown reason.
        reason: String,
    },
    /// A transaction signature was attached to an operator-visible flow.
    TransactionObserved {
        /// Event metadata.
        metadata: EventMetadata,
        /// Related trade, if any.
        trade_id: Option<TradeId>,
        /// Solana signature.
        signature: TxSignature,
    },
}

/// Always-on reconciliation worker observation event.
///
/// Emitted once per asset+scope per tick — even when the observed drift is
/// within dust or a guard skips the adjustment. Operator visibility comes
/// from `/v1/runtime/events`; the `outcome` discriminates whether the tick
/// posted a ledger adjustment.
///
/// Amounts are stringly-encoded to keep the event JSON-serializable across
/// the API boundary without introducing a `BigUint` dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReconciliationEvent {
    /// One reconciliation tick observation.
    Tick {
        /// Event metadata.
        metadata: EventMetadata,
        /// Asset that was observed.
        asset: AssetId,
        /// Whether the wallet or Gateway scope was reconciled.
        scope: ReconciliationScope,
        /// On-chain (or Gateway-reported) raw token amount, stringly-encoded.
        on_chain_raw: String,
        /// Expected raw token amount derived from the ledger, stringly-encoded.
        expected_raw: String,
        /// Signed drift `on_chain - expected`, stringly-encoded.
        drift_raw: String,
        /// Outcome of the tick.
        outcome: ReconciliationOutcome,
    },
}

/// Scope for a reconciliation tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationScope {
    /// Solana working-custody wallet reconciliation.
    Wallet,
    /// Circle Gateway balance reconciliation.
    Gateway,
}

/// Outcome of a reconciliation tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReconciliationOutcome {
    /// Drift was within the configured per-asset dust threshold.
    WithinDust,
    /// Drift was above dust but the consecutive-observation window has not
    /// yet been satisfied.
    Building {
        /// How many consecutive same-sign observations have been recorded.
        observations: u8,
    },
    /// Drift triggered a balanced ledger adjustment.
    Adjusted {
        /// Idempotency key under which the adjustment was persisted.
        idempotency_key: String,
    },
    /// Drift triggered an adjustment but a hard guard blocked it.
    Skipped {
        /// Stable, operator-facing reason.
        reason: String,
    },
}

/// Always-on automation worker tick event.
///
/// Emitted by each independent worker (rebalance, gateway refill, native
/// SOL top-up) once per tick. The `outcome` discriminates whether the tick
/// submitted any adapter action; downstream events
/// (`SwapEvent::*` / `GatewayEvent::*`) carry the actual ledger-affecting
/// movements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationEvent {
    /// One automation worker observation tick.
    Tick {
        /// Event metadata.
        metadata: EventMetadata,
        /// Which worker fired this tick.
        kind: AutomationKind,
        /// Outcome of the tick.
        outcome: AutomationOutcome,
    },
}

/// Worker kind discriminator for [`AutomationEvent::Tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationKind {
    /// Inventory rebalance worker.
    Rebalance,
    /// Circle Gateway USDC refill worker.
    GatewayRefill,
    /// Working USDC excess deposit worker.
    ExcessDeposit,
    /// Native SOL gas top-up worker.
    NativeTopUp,
}

/// Outcome of one automation worker tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationOutcome {
    /// The tick ran but no automation action was needed.
    NoActionNeeded,
    /// One or more automation actions were submitted on this tick.
    Submitted {
        /// Number of swap actions submitted.
        completed_swaps: usize,
        /// Number of Gateway refills submitted.
        completed_gateway_refills: usize,
        /// Number of Gateway excess deposits submitted.
        completed_gateway_deposits: usize,
    },
    /// The tick failed during automation execution.
    Failed {
        /// Operator-facing reason.
        reason: String,
    },
}
