//! Async adapter traits for future protocol implementations.

use async_trait::async_trait;

use crate::domain::events::RuntimeEvent;
use crate::domain::settlement::SettlementLeg;
use crate::domain::types::{
    AmountRaw, AssetId, AssetPair, BalanceSnapshot, ExternalHtlcInitiation, GatewayReceipt,
    GatewayRefillRequest, HtlcInitiation, HtlcReceipt, LedgerMovement, ReferencePrice,
    SettlementStatus, SwapQuote, SwapReceipt, SwapRequest, TradeId, TxSignature,
    UnsignedWalletTransaction, WalletAddress, WalletRole,
};
use crate::error::AppError;

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

/// Quotes and executes spot swaps used for rebalance actions.
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
    /// Return a configured server-owned wallet address.
    ///
    /// # Errors
    ///
    /// Returns an error when the role is not configured for this runtime.
    async fn wallet_address(&self, role: WalletRole) -> Result<WalletAddress, AppError> {
        let _ = role;
        Err(AppError::unsupported(
            "HTLC client does not expose wallet addresses",
        ))
    }

    /// Initiate an HTLC escrow for a trade.
    ///
    /// # Errors
    ///
    /// Returns an error when instruction construction, signing, submission, or
    /// confirmation fails.
    async fn initiate(&self, request: HtlcInitiation) -> Result<HtlcReceipt, AppError>;

    /// Initiate a server-funded HTLC whose redeemer is an external browser wallet.
    ///
    /// # Errors
    ///
    /// Returns an error when instruction construction, signing, submission, or
    /// confirmation fails.
    async fn initiate_with_external_redeemer(
        &self,
        request: HtlcInitiation,
        redeemer: WalletAddress,
    ) -> Result<HtlcReceipt, AppError> {
        let _ = request;
        let _ = redeemer;
        Err(AppError::unsupported(
            "HTLC client does not support external redeemers",
        ))
    }

    /// Build an unsigned browser-funded HTLC lock transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction cannot be constructed.
    async fn build_external_initiate(
        &self,
        request: ExternalHtlcInitiation,
    ) -> Result<UnsignedWalletTransaction, AppError> {
        let _ = request;
        Err(AppError::unsupported(
            "HTLC client does not support browser-funded locks",
        ))
    }

    /// Record a submitted browser-funded HTLC lock transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the signature cannot be validated or confirmed.
    async fn record_external_initiate(
        &self,
        request: ExternalHtlcInitiation,
        signature: TxSignature,
    ) -> Result<HtlcReceipt, AppError> {
        let _ = request;
        let _ = signature;
        Err(AppError::unsupported(
            "HTLC client does not support browser-funded lock recording",
        ))
    }

    /// Build an unsigned browser redeem transaction for a server-funded HTLC.
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction cannot be constructed.
    async fn build_external_redeem(
        &self,
        trade_id: TradeId,
        redeemer: WalletAddress,
        preimage: String,
    ) -> Result<UnsignedWalletTransaction, AppError> {
        let _ = trade_id;
        let _ = redeemer;
        let _ = preimage;
        Err(AppError::unsupported(
            "HTLC client does not support browser redeem construction",
        ))
    }

    /// Record a submitted browser redeem transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the signature cannot be validated or confirmed.
    async fn record_external_redeem(
        &self,
        trade_id: TradeId,
        signature: TxSignature,
    ) -> Result<HtlcReceipt, AppError> {
        let _ = trade_id;
        let _ = signature;
        Err(AppError::unsupported(
            "HTLC client does not support browser redeem recording",
        ))
    }

    /// Build an unsigned browser-funded HTLC refund transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction cannot be constructed.
    async fn build_external_refund(
        &self,
        trade_id: TradeId,
        funder: WalletAddress,
    ) -> Result<UnsignedWalletTransaction, AppError> {
        let _ = trade_id;
        let _ = funder;
        Err(AppError::unsupported(
            "HTLC client does not support browser refund construction",
        ))
    }

    /// Record a submitted browser refund transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the signature cannot be validated or confirmed.
    async fn record_external_refund(
        &self,
        trade_id: TradeId,
        signature: TxSignature,
    ) -> Result<HtlcReceipt, AppError> {
        let _ = trade_id;
        let _ = signature;
        Err(AppError::unsupported(
            "HTLC client does not support browser refund recording",
        ))
    }

    /// Refund one specific server-owned HTLC leg.
    ///
    /// # Errors
    ///
    /// Returns an error when the leg cannot be refunded.
    async fn refund_leg(
        &self,
        trade_id: TradeId,
        leg: SettlementLeg,
    ) -> Result<HtlcReceipt, AppError> {
        let _ = leg;
        self.refund(trade_id).await
    }

    /// Restore known settlement legs into an adapter's in-memory cache after
    /// process restart without submitting any transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the adapter cannot reconstruct leg metadata.
    async fn restore_wallet_settlement(
        &self,
        taker_lock: ExternalHtlcInitiation,
        maker_lock: HtlcInitiation,
        taker_locked: bool,
        maker_locked: bool,
    ) -> Result<(), AppError> {
        let _ = taker_lock;
        let _ = maker_lock;
        let _ = taker_locked;
        let _ = maker_locked;
        Ok(())
    }

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

    /// Deposit excess working USDC into Circle Gateway.
    ///
    /// # Errors
    ///
    /// Returns an error when the Solana Gateway deposit transaction fails or
    /// Gateway cannot confirm the resulting balance.
    async fn deposit(&self, amount_raw: AmountRaw) -> Result<GatewayReceipt, AppError> {
        let _ = amount_raw;
        Err(AppError::unsupported(
            "Gateway deposit is not supported by this adapter",
        ))
    }
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

/// Publishes runtime events to a stream, buffer, API projection, or web app.
#[async_trait]
pub trait RuntimeEventSink: Send + Sync {
    /// Publish a runtime event.
    ///
    /// # Errors
    ///
    /// Returns an error when the sink cannot accept or persist the event.
    async fn publish(&self, event: RuntimeEvent) -> Result<(), AppError>;
}

/// Records ledger movements through a generic adapter boundary.
#[async_trait]
pub trait LedgerSink: Send + Sync {
    /// Record a ledger entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger backend rejects or cannot persist the
    /// entry.
    async fn record(&self, entry: LedgerMovement) -> Result<(), AppError>;
}

/// Marker trait for adapters that need to validate pair support before work.
pub trait SupportsPair {
    /// Return true when the adapter supports this directional pair.
    #[must_use]
    fn supports_pair(&self, pair: &AssetPair) -> bool;
}
