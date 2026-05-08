//! JSON request and response contracts for the local operator API.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::application::runtime::{
    GatewayProjection, PnlProjection, RebalanceProjection, RfqProjection, RiskProjection,
    RuntimeState,
};
use crate::domain::events::RuntimeEvent;
use crate::domain::types::{
    AmountRaw, MintAddress, QuoteId, RejectionReason, SettlementStatus, TokenAmount, TradeId,
    TxSignature, UnsignedWalletTransaction, WalletAddress,
};

/// Request body for `POST /v1/rfq`.
///
/// Accepts two equivalent shapes for backward compatibility:
/// - **Friendly** (preferred): `input_asset`, `output_asset`, and a decimal
///   `amount` string. Asset identifiers are case-insensitive.
/// - **Legacy**: `input_mint`, `output_mint`, and `input_amount_raw` integer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RfqRequest {
    /// Friendly input asset symbol or id (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_asset: Option<String>,
    /// Friendly output asset symbol or id (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_asset: Option<String>,
    /// Friendly input amount in display units (e.g. `"0.01"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    /// Legacy mint address provided by the taker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_mint: Option<MintAddress>,
    /// Legacy mint address requested by the taker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_mint: Option<MintAddress>,
    /// Legacy raw input amount in the input mint's native decimals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_amount_raw: Option<AmountRaw>,
    /// Taker wallet address used by the settlement flow.
    pub taker_wallet: WalletAddress,
    /// Optional quote expiry override in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_seconds: Option<u64>,
}

/// Pair summary echoed on accepted RFQ responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RfqPair {
    /// Input asset id (canonical case).
    pub input_asset: String,
    /// Output asset id (canonical case).
    pub output_asset: String,
}

/// Display-friendly amount tuple returned alongside legacy raw fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmountView {
    /// Asset id (canonical case).
    pub asset: String,
    /// Decimal amount serialized as a string for precision-safe transport.
    pub amount: String,
    /// Raw integer amount serialized as a string.
    pub amount_raw: String,
    /// Asset native decimals.
    pub decimals: u8,
    /// Asset mint address. `None` for native SOL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mint: Option<MintAddress>,
}

/// Self-describing pointer to the next call an API consumer should make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextAction {
    /// Stable action identifier (e.g. `start_wallet_settlement`).
    #[serde(rename = "type")]
    pub kind: String,
    /// HTTP method.
    pub method: String,
    /// HTTP path with concrete identifiers substituted.
    pub path: String,
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
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// Hashlock commitment once settlement workers are integrated.
    pub hashlock: Option<String>,
}

/// Response body for `POST /v1/rfq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum RfqResponse {
    /// The runtime accepted the RFQ and produced firm quote terms.
    Accepted {
        /// Runtime quote identifier.
        quote_id: QuoteId,
        /// Quoted output amount in raw destination units. Legacy field.
        quoted_output_amount_raw: AmountRaw,
        /// Spread applied by the quote engine.
        spread_bps: u16,
        /// Quote expiry timestamp.
        #[serde(with = "time::serde::rfc3339")]
        expires_at: OffsetDateTime,
        /// HTLC acceptance terms.
        htlc_terms: HtlcAcceptanceTerms,
        /// Operator-readable checks that passed.
        risk_checks: Vec<String>,
        /// Integration status for the route handler.
        integration_status: IntegrationStatus,
        /// Display-friendly pair echo.
        pair: RfqPair,
        /// Display-friendly input amount.
        input: AmountView,
        /// Display-friendly output amount.
        output: AmountView,
        /// Self-describing next call for an API consumer.
        next_action: NextAction,
    },
    /// The RFQ was rejected by validation or risk.
    Rejected {
        /// Stable rejection reason.
        reason: RejectionReason,
        /// Operator-readable risk details.
        risk_check_details: Vec<String>,
        /// Integration status for the route handler.
        integration_status: IntegrationStatus,
        /// User-facing message describing the rejection.
        message: String,
        /// Concrete suggestion the caller can act on.
        suggested_action: String,
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

/// Public asset metadata returned to the web app and direct API consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetResponse {
    /// Configured asset identifier.
    pub id: String,
    /// Display symbol.
    pub symbol: String,
    /// Solana mint address used by RFQs.
    pub mint: MintAddress,
    /// Native token decimals.
    pub decimals: u8,
    /// Per-asset minimum user-entered trade amount in display units.
    pub min_trade_amount: Decimal,
    /// Per-asset maximum user-entered trade amount in display units.
    pub max_trade_amount: Decimal,
    /// Lower-case aliases accepted by friendly RFQ requests.
    pub aliases: Vec<String>,
    /// Asset class: `native` or `spl`.
    pub kind: String,
    /// Network identifier for clients listing multiple chains.
    pub network: String,
    /// Asset ids accepted as the output side of a directional pair.
    pub supported_outputs: Vec<String>,
    /// Minimum raw working inventory required before quoting this asset,
    /// serialized as a string so JSON consumers do not lose precision.
    pub quoteable_threshold_raw: String,
    /// Display-unit form of `quoteable_threshold_raw`.
    pub quoteable_threshold: String,
}

/// Response body for `GET /v1/pairs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairsResponse {
    /// Configured directional pairs and basic per-pair constraints.
    pub pairs: Vec<PairResponse>,
}

/// One directional pair plus its constraints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairResponse {
    /// Input asset id (canonical case).
    pub input_asset: String,
    /// Output asset id (canonical case).
    pub output_asset: String,
    /// Input mint address.
    pub input_mint: MintAddress,
    /// Output mint address.
    pub output_mint: MintAddress,
    /// Input asset decimals.
    pub input_decimals: u8,
    /// Output asset decimals.
    pub output_decimals: u8,
    /// Maximum quote notional in estimated USD (the smaller of the per-asset
    /// caps for both legs).
    pub max_quote_notional_usd: Decimal,
    /// Minimum quote notional in estimated USD (the larger of the per-asset
    /// minimums for both legs).
    pub min_quote_notional_usd: Decimal,
    /// Minimum user-entered input amount in display units.
    pub min_input_trade_amount: Decimal,
    /// Maximum user-entered input amount in display units.
    pub max_input_trade_amount: Decimal,
    /// Default expiry in seconds applied when a request omits `expiry_seconds`.
    pub default_expiry_seconds: u64,
}

/// Request body for `POST /v1/quotes/{quote_id}/wallet-settlement`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletSettlementRequest {
    /// Connected browser wallet that requested the quote.
    pub taker_wallet: WalletAddress,
    /// Hex-encoded SHA-256 preimage commitment generated by the browser.
    pub secret_hash: String,
}

/// Response body for wallet settlement start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletSettlementResponse {
    /// Quote accepted for wallet settlement.
    pub quote_id: QuoteId,
    /// Trade created for settlement tracking.
    pub trade_id: TradeId,
    /// Unsigned taker lock transaction for browser signing.
    pub taker_lock_transaction: UnsignedWalletTransaction,
    /// HTLC expiry timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// Integration status for the route handler.
    pub integration_status: IntegrationStatus,
    /// Self-describing next call for an API consumer.
    pub next_action: NextAction,
}

/// Request body for recording a browser taker lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakerLockRequest {
    /// Submitted taker lock transaction signature.
    pub signature: TxSignature,
}

/// Response body after maker lock submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakerLockResponse {
    /// Trade being settled.
    pub trade_id: TradeId,
    /// Current settlement status.
    pub settlement_status: SettlementStatus,
    /// Maker lock signature.
    pub maker_lock_signature: Option<TxSignature>,
    /// All observed settlement signatures.
    pub tx_signatures: Vec<TxSignature>,
    /// Integration status for the route handler.
    pub integration_status: IntegrationStatus,
    /// Self-describing next call for an API consumer.
    pub next_action: NextAction,
}

/// Request body for preparing or recording browser taker redeem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakerRedeemRequest {
    /// Hex-encoded preimage revealed at redeem time.
    pub preimage: String,
    /// Submitted taker redeem transaction signature. Omit to request the
    /// unsigned redeem transaction first.
    pub signature: Option<TxSignature>,
}

/// Response body for the taker redeem route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakerRedeemResponse {
    /// Trade being settled.
    pub trade_id: TradeId,
    /// Current settlement status.
    pub settlement_status: SettlementStatus,
    /// Unsigned taker redeem transaction when signature is omitted.
    pub taker_redeem_transaction: Option<UnsignedWalletTransaction>,
    /// Maker redeem signature after finalization.
    pub maker_redeem_signature: Option<TxSignature>,
    /// All observed settlement signatures after finalization.
    pub tx_signatures: Vec<TxSignature>,
    /// Ledger summary.
    pub ledger_summary: LedgerSummary,
    /// Integration status for the route handler.
    pub integration_status: IntegrationStatus,
    /// Self-describing next call for an API consumer. `None` once the trade
    /// is fully redeemed and no further action is required.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<NextAction>,
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
    /// Known trade amounts. Legacy envelope.
    pub amounts: TradeAmounts,
    /// Display-friendly input amount.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<AmountView>,
    /// Display-friendly output amount.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<AmountView>,
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

/// Request body for admin login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminLoginRequest {
    /// Configured admin username.
    pub username: String,
    /// Plaintext password verified against the configured Argon2id PHC hash.
    pub password: String,
}

/// Response body for admin identity checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminMeResponse {
    /// Whether an admin session is currently valid.
    pub authenticated: bool,
    /// Authenticated username, when present.
    pub username: Option<String>,
    /// Session expiry, when authenticated.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

/// Protected admin dashboard summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminSummaryResponse {
    /// Current inventory projection.
    pub inventory: crate::application::runtime::InventoryProjection,
    /// Current risk projection.
    pub risk: RiskProjection,
    /// Current P&L projection.
    pub pnl: PnlProjection,
    /// RFQ counters and settlement count.
    pub rfq: RfqProjection,
    /// Rebalance counters.
    pub rebalance: RebalanceProjection,
    /// Circle Gateway state.
    pub gateway: GatewayProjection,
    /// Recent runtime events.
    pub recent_events: Vec<RuntimeEvent>,
}

/// Response body for `GET /v1/runtime/ledger`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerSnapshotResponse {
    /// Whether the ledger is healthy: integrity report passes and no
    /// protected account holds a negative balance.
    pub healthy: bool,
    /// Total ledger entry count (whole ledger, ignores filtering).
    pub entry_count: u64,
    /// Non-zero derived balances. Filtered subset when `account_type` is
    /// passed.
    pub balances: Vec<LedgerBalanceEntry>,
}

/// One derived ledger balance row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerBalanceEntry {
    /// Snake-case account-type string from
    /// [`crate::adapters::persistence::ledger::LedgerAccountType`].
    pub account_type: String,
    /// Asset identifier (e.g. `USDC`).
    pub asset: String,
    /// Optional account qualifier echoed verbatim from the entry. Never
    /// includes a Solana wallet address.
    pub qualifier: Option<String>,
    /// Stringified `i128` raw amount; signed.
    pub balance_raw: String,
    /// Asset native decimals from the registry, or `0` when unknown.
    pub decimals: u8,
    /// Fixed-width display amount with `decimals` fractional digits.
    pub display_amount: String,
}

/// Response body for `GET /v1/runtime/trades`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradesResponse {
    /// Total in-memory trade count.
    pub total_count: u64,
    /// Total trades whose settlement status is `redeemed`.
    pub successful_count: u64,
    /// Trades returned, newest first, capped at `limit` (default 10, max 100).
    pub trades: Vec<TradeSummary>,
}

/// One trade summary row returned by `/v1/runtime/trades`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeSummary {
    /// Runtime trade identifier.
    pub trade_id: String,
    /// Quote that produced the trade.
    pub quote_id: String,
    /// Snake-case settlement status.
    pub settlement_status: String,
    /// Taker input amount.
    pub input: TradeAmount,
    /// Maker output amount.
    pub output: TradeAmount,
    /// Trade-bound transaction signatures with `kind` discriminator.
    /// Background rebalance signatures are excluded.
    pub tx_signatures: Vec<TradeSignature>,
}

/// Per-trade input/output amount breakdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeAmount {
    /// Asset identifier.
    pub asset: String,
    /// Stringified `i128` raw amount.
    pub amount_raw: String,
    /// Asset native decimals from the registry, or `0` when unknown.
    pub decimals: u8,
    /// Fixed-width display amount with `decimals` fractional digits.
    pub display_amount: String,
}

pub use crate::application::runtime::{TradeSignature, TradeSignatureKind};

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
