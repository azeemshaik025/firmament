//! JSON request and response contracts for the local operator API.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::application::runtime::RuntimeState;
use crate::domain::events::RuntimeEvent;
use crate::domain::types::{
    AmountRaw, MintAddress, QuoteId, RejectionReason, SettlementStatus, TokenAmount, TradeId,
    TxSignature, WalletAddress,
};

/// Request body for `POST /v1/rfq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RfqRequest {
    /// Mint address provided by the taker.
    pub input_mint: MintAddress,
    /// Mint address requested by the taker.
    pub output_mint: MintAddress,
    /// Raw input amount in the input mint's native decimals.
    pub input_amount_raw: AmountRaw,
    /// Taker wallet address used by the demo settlement flow.
    pub taker_wallet: WalletAddress,
    /// Optional quote expiry override in seconds.
    pub expiry_seconds: Option<u64>,
}

/// API integration status for route handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationStatus {
    /// The response came from the runtime orchestrator and adapter boundary.
    RuntimeOrchestrated,
}

/// HTLC terms returned with an accepted RFQ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtlcAcceptanceTerms {
    /// Settlement model label for downstream clients.
    pub settlement_model: String,
    /// Mint address expected to be escrowed by the settlement flow.
    pub escrow_mint: MintAddress,
    /// HTLC expiry timestamp.
    pub expires_at: OffsetDateTime,
    /// Hashlock commitment once settlement workers are integrated.
    pub hashlock: Option<String>,
}

/// Response body for `POST /v1/rfq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RfqResponse {
    /// The runtime accepted the RFQ and produced firm quote terms.
    Accepted {
        /// Runtime quote identifier.
        quote_id: QuoteId,
        /// Quoted output amount in raw destination units.
        quoted_output_amount_raw: AmountRaw,
        /// Spread applied by the quote engine.
        spread_bps: u16,
        /// Quote expiry timestamp.
        expires_at: OffsetDateTime,
        /// HTLC acceptance terms.
        htlc_terms: HtlcAcceptanceTerms,
        /// Operator-readable checks that passed.
        risk_checks: Vec<String>,
        /// Integration status for the route handler.
        integration_status: IntegrationStatus,
    },
    /// The RFQ was rejected by validation or risk.
    Rejected {
        /// Stable rejection reason.
        reason: RejectionReason,
        /// Operator-readable risk details.
        risk_check_details: Vec<String>,
        /// Integration status for the route handler.
        integration_status: IntegrationStatus,
    },
}

/// Response body for `POST /v1/quotes/{quote_id}/accept`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteAcceptResponse {
    /// Quote accepted by the operator.
    pub quote_id: QuoteId,
    /// Trade created for settlement tracking.
    pub trade_id: TradeId,
    /// Current settlement status.
    pub settlement_status: SettlementStatus,
    /// Solana transaction signatures observed so far.
    pub tx_signatures: Vec<TxSignature>,
    /// Ledger summary once persistence is integrated.
    pub ledger_summary: LedgerSummary,
    /// Integration status for the route handler.
    pub integration_status: IntegrationStatus,
}

/// Token amounts associated with a trade lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeAmounts {
    /// Taker input amount when known.
    pub input: Option<TokenAmount>,
    /// Maker output amount when known.
    pub output: Option<TokenAmount>,
}

/// Small ledger summary returned by trade and accept responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerSummary {
    /// Whether known double-entry movements balance.
    pub balanced: bool,
    /// Count of ledger entries included in the summary.
    pub entry_count: usize,
    /// Net USDC estimate when P&L projection is integrated.
    pub net_usdc_estimate: Decimal,
}

impl Default for LedgerSummary {
    fn default() -> Self {
        Self {
            balanced: true,
            entry_count: 0,
            net_usdc_estimate: Decimal::ZERO,
        }
    }
}

/// Response body for `GET /v1/trades/{trade_id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeResponse {
    /// Requested trade identifier.
    pub trade_id: TradeId,
    /// Current settlement status.
    pub settlement_status: SettlementStatus,
    /// Solana transaction signatures observed so far.
    pub tx_signatures: Vec<TxSignature>,
    /// Known trade amounts.
    pub amounts: TradeAmounts,
    /// Ledger summary for the trade.
    pub ledger_summary: LedgerSummary,
    /// Integration status for the route handler.
    pub integration_status: IntegrationStatus,
}

/// Response body for `GET /v1/runtime/state`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStateResponse {
    /// Current runtime read projection.
    pub state: RuntimeState,
}

/// Response body for `GET /v1/runtime/events`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEventsResponse {
    /// Number of events returned.
    pub count: usize,
    /// Recent runtime events in chronological order.
    pub events: Vec<RuntimeEvent>,
}

/// Stable JSON error envelope used by API routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Error payload.
    pub error: ErrorBody,
}

/// Stable JSON error payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Machine-readable error code.
    pub code: String,
    /// Operator-facing message.
    pub message: String,
    /// Non-secret error details.
    pub details: Vec<String>,
}
