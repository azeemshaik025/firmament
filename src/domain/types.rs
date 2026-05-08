//! Domain value objects shared across runtime boundaries.

use std::fmt::{self, Display, Formatter};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Stable identifier for a supported asset such as USDC, SOL, or cbBTC.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetId(String);

impl AssetId {
    /// Create an asset identifier from a symbol or protocol identifier.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the inner string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AssetId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl Display for AssetId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Solana mint address wrapper used by config, adapters, and API responses.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MintAddress(String);

impl MintAddress {
    /// Create a mint address value object.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the inner address string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for MintAddress {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Solana wallet address wrapper for maker, taker, and Gateway destinations.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WalletAddress(String);

impl WalletAddress {
    /// Create a wallet address value object.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the inner address string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for WalletAddress {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<&str> for WalletAddress {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Role of a configured wallet within the demo runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalletRole {
    /// Maker/operator wallet that owns working liquidity.
    Maker,
    /// Taker/app wallet used for the two-wallet settlement demo.
    Taker,
    /// Local operator identity used for administration.
    Operator,
    /// Circle Gateway settlement or refill address.
    Gateway,
}

/// Raw integer token amount in the asset's native decimals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AmountRaw(u64);

impl AmountRaw {
    /// Create a raw token amount.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the integer amount.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// True when the amount is exactly zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

/// A raw amount tagged with its asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAmount {
    /// Asset being measured.
    pub asset: AssetId,
    /// Raw token units for the asset.
    pub amount_raw: AmountRaw,
}

impl TokenAmount {
    /// Create an asset-tagged token amount.
    #[must_use]
    pub fn new(asset: AssetId, amount_raw: AmountRaw) -> Self {
        Self { asset, amount_raw }
    }
}

/// Directional asset pair for RFQs, swaps, and price lookups.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetPair {
    /// Asset provided by the taker or swap source.
    pub input: AssetId,
    /// Asset returned by the maker or swap destination.
    pub output: AssetId,
}

impl AssetPair {
    /// Create a directional asset pair.
    #[must_use]
    pub fn new(input: AssetId, output: AssetId) -> Self {
        Self { input, output }
    }
}

/// Runtime quote identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QuoteId(Uuid);

impl QuoteId {
    /// Generate a time-sortable quote identifier.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    /// Borrow the underlying UUID.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Display for QuoteId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

/// Runtime trade identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TradeId(Uuid);

impl TradeId {
    /// Generate a time-sortable trade identifier.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    /// Borrow the underlying UUID.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Display for TradeId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

/// Identifier for a single runtime process invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeRunId(Uuid);

impl RuntimeRunId {
    /// Generate a time-sortable runtime run identifier.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    /// Borrow the underlying UUID.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Display for RuntimeRunId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

/// Solana transaction signature wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxSignature(String);

impl TxSignature {
    /// Create a transaction signature value object.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the signature string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for TxSignature {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Structured outcome of a risk evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RiskDecision {
    /// Risk checks passed and the runtime may continue.
    Accepted {
        /// Human-readable check labels that passed.
        checks: Vec<String>,
    },
    /// Risk checks failed and the runtime must reject the action.
    Rejected {
        /// Stable rejection reason for API, web app, and ledger consumers.
        reason: RejectionReason,
        /// Human-readable details for the operator.
        details: Vec<String>,
    },
}

/// Stable rejection reasons used by risk checks and operator views.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    /// The requested pair is not configured for quoting or swapping.
    UnsupportedPair,
    /// The asset is not configured or is disabled.
    UnsupportedAsset,
    /// The requested amount is zero or below the configured floor.
    AmountTooSmall,
    /// The requested notional exceeds a per-action cap.
    MaxNotionalExceeded,
    /// The cumulative automation cap would be exceeded.
    CumulativeCapExceeded,
    /// Inventory is below the configured quoteable threshold.
    InventoryBelowQuoteableThreshold,
    /// Configured exposure exceeds risk limits.
    ExposureLimitExceeded,
    /// Reference price data is too old to quote safely.
    StalePrice,
    /// The taker wallet is not currently allowed.
    WalletNotAllowed,
    /// Gateway state prevents a refill or settlement action.
    GatewayUnavailable,
    /// A required external service is unavailable.
    ExternalServiceUnavailable,
    /// Request shape or local policy validation failed.
    ValidationFailed,
}

/// Reference price snapshot from a pricing adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferencePrice {
    /// Pair the price applies to.
    pub pair: AssetPair,
    /// Output units per one input unit, expressed in decimal UI units.
    pub output_per_input: Decimal,
    /// Timestamp supplied by or assigned to the price sample.
    pub observed_at: OffsetDateTime,
}

/// Request passed to a swap adapter for quote or execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapRequest {
    /// Directional pair to swap.
    pub pair: AssetPair,
    /// Raw input amount.
    pub input_amount: TokenAmount,
    /// Wallet providing the input funds.
    pub source_wallet: WalletRole,
    /// Wallet receiving output funds.
    pub destination_wallet: WalletRole,
    /// Slippage cap in basis points.
    pub max_slippage_bps: u16,
}

/// Quote returned by a swap adapter before execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapQuote {
    /// Original swap request.
    pub request: SwapRequest,
    /// Expected raw output amount.
    pub expected_output: TokenAmount,
    /// Provider-estimated fee, if known.
    pub estimated_fee: Option<TokenAmount>,
    /// Quote expiry timestamp, if supplied by the provider.
    pub expires_at: Option<OffsetDateTime>,
}

/// Receipt returned after a swap adapter submits or confirms a transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapReceipt {
    /// Trade associated with the swap, when applicable.
    pub trade_id: Option<TradeId>,
    /// Submitted Solana transaction signature.
    pub signature: TxSignature,
    /// Actual output amount, if known.
    pub output_amount: Option<TokenAmount>,
}

/// HTLC initiation request used by the settlement adapter boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtlcInitiation {
    /// Trade this escrow belongs to.
    pub trade_id: TradeId,
    /// Wallet funding the escrow.
    pub funder: WalletRole,
    /// Wallet allowed to redeem the escrow.
    pub redeemer: WalletRole,
    /// Asset and amount placed into escrow.
    pub amount: TokenAmount,
    /// Hash preimage commitment encoded by the future HTLC client.
    pub hashlock: String,
    /// Refund deadline.
    pub expires_at: OffsetDateTime,
}

/// HTLC initiation request where the funding wallet is an external browser
/// wallet instead of a server-owned configured wallet role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalHtlcInitiation {
    /// Trade this escrow belongs to.
    pub trade_id: TradeId,
    /// Browser wallet funding the escrow.
    pub funder: WalletAddress,
    /// Wallet allowed to redeem the escrow.
    pub redeemer: WalletAddress,
    /// Asset and amount placed into escrow.
    pub amount: TokenAmount,
    /// Hash preimage commitment encoded by the future HTLC client.
    pub hashlock: String,
    /// Refund deadline.
    pub expires_at: OffsetDateTime,
}

/// Serialized unsigned transaction returned to a browser wallet for signing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsignedWalletTransaction {
    /// Base64-encoded Solana transaction bytes.
    pub transaction_base64: String,
    /// Latest blockhash used by the transaction.
    pub recent_blockhash: String,
}

/// Current high-level status of a settlement flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementStatus {
    /// Settlement has been requested but not submitted.
    Pending,
    /// Escrow transaction has been submitted or confirmed.
    Initiated,
    /// Escrow has been redeemed.
    Redeemed,
    /// Escrow has been refunded after expiry.
    Refunded,
    /// Settlement failed before a terminal on-chain state.
    Failed,
}

/// Receipt returned by an HTLC adapter.
///
/// `leg` identifies WHICH HTLC the receipt describes — not who acted on it.
/// On `Initiated` receipts the funder of the leg performed the action; on
/// `Redeemed` receipts the leg's redeemer performed the action; on `Refunded`
/// receipts the funder reclaimed it. So a `Redeemed` receipt with
/// `leg = MakerOutput` means "the maker's output HTLC was redeemed (by the
/// taker)", while `leg = TakerInput` means "the taker's input HTLC was
/// redeemed (by the maker)". This convention keeps the field stable across
/// the settlement lifecycle and lets ledger consumers size debits/credits
/// against the correct HTLC's amount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtlcReceipt {
    /// Trade this receipt belongs to.
    pub trade_id: TradeId,
    /// Which HTLC leg this receipt describes (see type-level docs for the
    /// "whose HTLC" vs "who acted" convention).
    pub leg: crate::domain::settlement::SettlementLeg,
    /// Asset and amount escrowed by the leg this receipt describes.
    pub amount: TokenAmount,
    /// Updated settlement status.
    pub status: SettlementStatus,
    /// Related Solana transaction signature.
    pub signature: Option<TxSignature>,
}

/// Gateway refill request for moving USDC into the working wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayRefillRequest {
    /// Requested USDC refill amount in raw units.
    pub amount: TokenAmount,
    /// Destination wallet for the refill.
    pub destination: WalletRole,
}

/// Receipt returned by a Gateway adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayReceipt {
    /// Asset and amount moved or checked.
    pub amount: TokenAmount,
    /// Provider transfer identifier, if available.
    pub provider_transfer_id: Option<String>,
    /// Solana transaction signature, if submitted.
    pub signature: Option<TxSignature>,
}

/// Balance snapshot for one wallet role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    /// Wallet role represented by the snapshot.
    pub wallet: WalletRole,
    /// Token balances known at snapshot time.
    pub balances: Vec<TokenAmount>,
    /// Snapshot timestamp.
    pub observed_at: OffsetDateTime,
}

/// Lightweight ledger movement event used by generic ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerMovement {
    /// Trade associated with the entry, if any.
    pub trade_id: Option<TradeId>,
    /// Stable ledger category.
    pub category: LedgerEntryCategory,
    /// Amount moved by this entry, if token-denominated.
    pub amount: Option<TokenAmount>,
    /// Optional operator-facing note.
    pub note: Option<String>,
    /// Entry timestamp.
    pub recorded_at: OffsetDateTime,
}

/// Coarse ledger categories for early integration work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerEntryCategory {
    /// Quote lifecycle record.
    Quote,
    /// Fill or inventory movement.
    Fill,
    /// HTLC escrow movement.
    HtlcEscrow,
    /// Fees paid or accrued.
    Fees,
    /// Rebalance movement.
    Rebalance,
    /// P&L estimate movement.
    ProfitAndLoss,
}

/// How the runtime intends to source the maker output for a quote/trade.
///
/// Determined at quote-issue time and attached to the in-memory `Quote` and
/// `RuntimeTrade`. Used by the `LedgerEventConsumer` to branch lifecycle
/// movements between the inventory-only path and the Gateway-backed path
/// (which involves Gateway burn/mint and, for non-USDC outputs, a Jupiter
/// swap).
///
/// This is in-memory only — trades are not yet persisted across restart, so
/// the path is recovered only for the lifetime of the runtime process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPath {
    /// Maker `working_custody` covers the requested output. No Gateway or DEX
    /// activity is needed for the lifecycle.
    InventoryToInventory,
    /// Maker pulls USDC from the Circle Gateway and (when output is not USDC)
    /// swaps it on Jupiter into the target asset before the maker leg.
    GatewayToDex,
}
