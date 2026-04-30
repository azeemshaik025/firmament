//! Async adapter traits for future protocol implementations.

use async_trait::async_trait;

use crate::error::AppError;
use crate::events::RuntimeEvent;
use crate::types::{
    AssetId, AssetPair, BalanceSnapshot, GatewayReceipt, GatewayRefillRequest, HtlcInitiation,
    HtlcReceipt, LedgerEntry, ReferencePrice, SettlementStatus, SwapQuote, SwapReceipt,
    SwapRequest, TradeId, WalletRole,
};

/// Provides reference prices for quote construction and P&L estimates.
#[async_trait]
pub trait PriceProvider: Send + Sync {
    /// Return a reference price for a directional pair.
    ///
    /// # Errors
    ///
    /// Returns an error when the pair is unsupported or the provider cannot
    /// produce fresh price data.
    async fn reference_price(&self, pair: AssetPair) -> Result<ReferencePrice, AppError>;
}

/// Quotes and executes spot swaps used for rebalance and hedge actions.
#[async_trait]
pub trait SwapExecutor: Send + Sync {
    /// Return an executable swap quote without submitting a transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when quoting fails, the route is unavailable, or local
    /// policy rejects the request.
    async fn quote_swap(&self, request: SwapRequest) -> Result<SwapQuote, AppError>;

    /// Execute a previously quoted swap.
    ///
    /// # Errors
    ///
    /// Returns an error when transaction construction, signing, submission, or
    /// confirmation fails.
    async fn execute_swap(&self, quote: SwapQuote) -> Result<SwapReceipt, AppError>;
}

/// Initiates, redeems, refunds, and queries Solana HTLC settlement flows.
#[async_trait]
pub trait HtlcClient: Send + Sync {
    /// Initiate an HTLC escrow for a trade.
    ///
    /// # Errors
    ///
    /// Returns an error when instruction construction, signing, submission, or
    /// confirmation fails.
    async fn initiate(&self, request: HtlcInitiation) -> Result<HtlcReceipt, AppError>;

    /// Redeem an HTLC escrow using a preimage.
    ///
    /// # Errors
    ///
    /// Returns an error when the preimage is invalid or the redeem transaction
    /// fails.
    async fn redeem(&self, trade_id: TradeId, preimage: String) -> Result<HtlcReceipt, AppError>;

    /// Refund an expired HTLC escrow.
    ///
    /// # Errors
    ///
    /// Returns an error when the escrow is not refundable or the refund
    /// transaction fails.
    async fn refund(&self, trade_id: TradeId) -> Result<HtlcReceipt, AppError>;

    /// Query the current settlement status for a trade.
    ///
    /// # Errors
    ///
    /// Returns an error when account lookup or decoding fails.
    async fn status(&self, trade_id: TradeId) -> Result<SettlementStatus, AppError>;
}

/// Reads Circle Gateway balances and requests USDC refills.
#[async_trait]
pub trait GatewayClient: Send + Sync {
    /// Return the current Gateway balance for an asset.
    ///
    /// # Errors
    ///
    /// Returns an error when Gateway credentials, connectivity, or response
    /// parsing fail.
    async fn balance(&self, asset: AssetId) -> Result<GatewayReceipt, AppError>;

    /// Request a Gateway refill into the configured working wallet.
    ///
    /// # Errors
    ///
    /// Returns an error when Gateway rejects the transfer or submission fails.
    async fn request_refill(
        &self,
        request: GatewayRefillRequest,
    ) -> Result<GatewayReceipt, AppError>;
}

/// Reads token balances for configured runtime wallets.
#[async_trait]
pub trait BalanceReader: Send + Sync {
    /// Return balances for a configured wallet role.
    ///
    /// # Errors
    ///
    /// Returns an error when wallet lookup, ATA lookup, or balance decoding
    /// fails.
    async fn balances(&self, wallet: WalletRole) -> Result<BalanceSnapshot, AppError>;
}

/// Publishes runtime events to a stream, buffer, API projection, or TUI.
#[async_trait]
pub trait RuntimeEventSink: Send + Sync {
    /// Publish a runtime event.
    ///
    /// # Errors
    ///
    /// Returns an error when the sink cannot accept or persist the event.
    async fn publish(&self, event: RuntimeEvent) -> Result<(), AppError>;
}

/// Records ledger movements without committing to a database schema in Wave 0.
#[async_trait]
pub trait LedgerSink: Send + Sync {
    /// Record a ledger entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger backend rejects or cannot persist the
    /// entry.
    async fn record(&self, entry: LedgerEntry) -> Result<(), AppError>;
}

/// Marker trait for adapters that need to validate pair support before work.
pub trait SupportsPair {
    /// Return true when the adapter supports this directional pair.
    #[must_use]
    fn supports_pair(&self, pair: &AssetPair) -> bool;
}
