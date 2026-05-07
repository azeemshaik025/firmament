//! Runtime event contracts consumed by API, web app, and ledger projections.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::types::{
    AssetPair, BalanceSnapshot, GatewayReceipt, HtlcReceipt, QuoteId, ReferencePrice,
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
