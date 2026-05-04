//! Event-driven runtime orchestration.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::RwLock;

use crate::adapters::persistence::db::Db;
use crate::adapters::persistence::ledger::{LedgerEventConsumer, SqliteLedgerRepository};
use crate::adapters::persistence::pnl::{PnlCategory, PnlEstimate, PnlPriceBook, PnlRepository};
use crate::adapters::solana::htlc::{generate_secret, hash_secret};
use crate::application::rebalance::{
    ActionKey, AutomationActionKind, AutomationLimits, AutomationState, DecisionBlockReason,
    GatewayRefillDecision, GatewayRefillPlan, PlannedSwap, RebalanceDecision, RebalancePolicy,
    decide_gateway_refill, decide_rebalance, submit_gateway_refill, submit_planned_swap,
};
use crate::application::rfq::{
    FirmQuote, QuoteAcceptance, RfqContext, RfqRequest, RfqResponse, accept_quote_for_settlement,
    request_quote,
};
use crate::config::AppConfig;
use crate::domain::assets::AssetRegistry;
use crate::domain::events::{
    EventMetadata, GatewayEvent, InventoryEvent, RuntimeEvent, SettlementEvent, SwapEvent,
    SystemEvent,
};
use crate::domain::inventory::{InventoryPolicy, InventorySnapshot as ValuedInventorySnapshot};
use crate::domain::quote_engine::InventorySnapshot as QuoteInventorySnapshot;
use crate::domain::settlement::{SettlementTerms, TwoSidedSettlement};
use crate::domain::types::{
    AmountRaw, AssetId, BalanceSnapshot, ExternalHtlcInitiation, QuoteId, SettlementStatus,
    TokenAmount, TradeId, TxSignature, UnsignedWalletTransaction, WalletAddress, WalletRole,
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

impl Default for RuntimeLedgerSummary {
    fn default() -> Self {
        Self {
            balanced: true,
            entry_count: 0,
            net_usdc_estimate: Decimal::ZERO,
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
    /// Latest settlement status.
    pub settlement_status: SettlementStatus,
    /// Solana signatures observed during settlement.
    pub tx_signatures: Vec<TxSignature>,
    /// Taker input amount.
    pub input_amount: TokenAmount,
    /// Maker output amount.
    pub output_amount: TokenAmount,
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

#[derive(Debug, Clone)]
struct WalletSettlementState {
    quote: FirmQuote,
    settlement: TwoSidedSettlement,
    taker_lock_request: ExternalHtlcInitiation,
    taker_wallet: WalletAddress,
    tx_signatures: Vec<TxSignature>,
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

/// Event-driven runtime flow coordinator.
#[derive(Clone)]
pub struct RuntimeOrchestrator {
    app_state: AppState,
    adapters: RuntimeAdapters,
    quotes: Arc<RwLock<HashMap<QuoteId, FirmQuote>>>,
    trades: Arc<RwLock<HashMap<TradeId, RuntimeTrade>>>,
    wallet_settlements: Arc<RwLock<HashMap<TradeId, WalletSettlementState>>>,
    automation_state: Arc<RwLock<AutomationState>>,
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
        let mut orchestrator = Self::new(app_state, adapters, options);
        orchestrator.persistence = Some(persistence);
        orchestrator
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

    /// Request a firm quote and store it only when risk accepts the RFQ.
    ///
    /// # Errors
    ///
    /// Returns balance-reader or event-publish errors. Risk rejections are
    /// returned as successful [`RfqResponse::Rejected`] values.
    pub async fn request_rfq(&self, request: RfqRequest) -> AppResult<RfqResponse> {
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

        let context = RfqContext::from_config(
            self.app_state.config(),
            quote_inventory_from_balance(self.app_state.config(), &balance_snapshot),
        );
        let now = OffsetDateTime::now_utc();
        let outcome = request_quote(
            request,
            &context,
            self.adapters.price_provider.as_ref(),
            self.app_state.run_id(),
            now,
        )
        .await;

        let response = outcome.response;
        for event in outcome.events {
            self.publish(event).await?;
        }

        if let RfqResponse::Accepted(quote) = &response {
            self.record_usd_price(&quote.reference_price).await;
            self.publish(RuntimeEvent::Swap(SwapEvent::PriceObserved {
                metadata: EventMetadata::new(self.app_state.run_id()),
                price: quote.reference_price.clone(),
            }))
            .await?;
            self.quotes
                .write()
                .await
                .insert(quote.quote_id, *quote.clone());
            *self.last_accepted_quote.write().await = Some(quote.quote_id);
        }

        Ok(response)
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
        let tx_signatures = self.settle_quote(&quote, trade_id).await?;
        let trade = RuntimeTrade {
            quote_id,
            trade_id,
            settlement_status: SettlementStatus::Redeemed,
            tx_signatures,
            input_amount: quote.input_amount.clone(),
            output_amount: quote.output_amount.clone(),
        };
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
    ) -> AppResult<Vec<TxSignature>> {
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

        self.run_settlement_locks(&mut settlement, quote, &mut tx_signatures)
            .await?;
        self.run_settlement_redeems(&mut settlement, quote, &mut tx_signatures, preimage)
            .await?;
        self.publish(RuntimeEvent::Settlement(SettlementEvent::StatusChanged {
            metadata: EventMetadata::new(self.app_state.run_id()),
            trade_id,
            status: SettlementStatus::Redeemed,
        }))
        .await?;

        Ok(tx_signatures)
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

        self.wallet_settlements.write().await.insert(
            trade_id,
            WalletSettlementState {
                quote,
                settlement,
                taker_lock_request,
                taker_wallet,
                tx_signatures: Vec::new(),
            },
        );

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
        let mut state = self.wallet_settlement_state(trade_id).await?;
        let taker_lock = self
            .adapters
            .htlc_client
            .record_external_initiate(state.taker_lock_request.clone(), taker_lock_signature)
            .await?;
        push_signature(&mut state.tx_signatures, taker_lock.signature.as_ref());
        let transition = state.settlement.record_taker_lock(
            self.app_state.run_id(),
            taker_lock.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await?;

        let maker_lock = self
            .adapters
            .htlc_client
            .initiate_with_external_redeemer(
                state.settlement.terms.maker_lock_request(),
                state.taker_wallet.clone(),
            )
            .await?;
        push_signature(&mut state.tx_signatures, maker_lock.signature.as_ref());
        let transition = state.settlement.record_maker_lock(
            self.app_state.run_id(),
            maker_lock.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await?;

        let response = WalletTakerLockResult {
            trade_id,
            settlement_status: SettlementStatus::Initiated,
            maker_lock_signature: maker_lock.signature,
            tx_signatures: state.tx_signatures.clone(),
        };
        self.wallet_settlements
            .write()
            .await
            .insert(trade_id, state);
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
        let mut state = self.wallet_settlement_state(trade_id).await?;
        let taker_redeem = self
            .adapters
            .htlc_client
            .record_external_redeem(trade_id, taker_redeem_signature)
            .await?;
        push_signature(&mut state.tx_signatures, taker_redeem.signature.as_ref());
        let transition = state.settlement.record_taker_redeem(
            self.app_state.run_id(),
            taker_redeem.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await?;

        let maker_redeem = self.adapters.htlc_client.redeem(trade_id, preimage).await?;
        push_signature(&mut state.tx_signatures, maker_redeem.signature.as_ref());
        let transition = state.settlement.record_maker_redeem(
            self.app_state.run_id(),
            maker_redeem.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await?;
        self.publish(RuntimeEvent::Settlement(SettlementEvent::StatusChanged {
            metadata: EventMetadata::new(self.app_state.run_id()),
            trade_id,
            status: SettlementStatus::Redeemed,
        }))
        .await?;

        let trade = RuntimeTrade {
            quote_id: state.quote.quote_id,
            trade_id,
            settlement_status: SettlementStatus::Redeemed,
            tx_signatures: state.tx_signatures.clone(),
            input_amount: state.quote.input_amount,
            output_amount: state.quote.output_amount,
        };
        self.trades.write().await.insert(trade_id, trade.clone());
        self.wallet_settlements.write().await.remove(&trade_id);
        self.refresh_inventory_and_automation().await?;

        Ok(WalletTakerRedeemResult {
            trade,
            maker_redeem_signature: maker_redeem.signature,
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

    async fn run_settlement_locks(
        &self,
        settlement: &mut TwoSidedSettlement,
        quote: &FirmQuote,
        tx_signatures: &mut Vec<TxSignature>,
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
        push_signature(tx_signatures, taker_lock.signature.as_ref());
        let transition = settlement.record_taker_lock(
            self.app_state.run_id(),
            taker_lock.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await?;

        let status = self
            .adapters
            .htlc_client
            .status(settlement.terms.trade_id)
            .await?;
        if status != SettlementStatus::Initiated {
            let reason = format!("taker HTLC validation returned {status:?}");
            self.record_settlement_failure(settlement, quote, tx_signatures, &reason)
                .await?;
            return Err(AppError::solana(reason));
        }

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
        push_signature(tx_signatures, maker_lock.signature.as_ref());
        let transition = settlement.record_maker_lock(
            self.app_state.run_id(),
            maker_lock.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await
    }

    async fn run_settlement_redeems(
        &self,
        settlement: &mut TwoSidedSettlement,
        quote: &FirmQuote,
        tx_signatures: &mut Vec<TxSignature>,
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
        push_signature(tx_signatures, taker_redeem.signature.as_ref());
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
        push_signature(tx_signatures, maker_redeem.signature.as_ref());
        let transition = settlement.record_maker_redeem(
            self.app_state.run_id(),
            maker_redeem.signature.as_ref().map(ToString::to_string),
        );
        self.publish(RuntimeEvent::Settlement(transition.event))
            .await
    }

    /// Return a stored trade record.
    pub async fn trade(&self, trade_id: TradeId) -> Option<RuntimeTrade> {
        self.trades.read().await.get(&trade_id).cloned()
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
    /// # Errors
    ///
    /// Returns balance-reader or projection errors. Individual automation
    /// adapter failures are emitted as runtime events and summarized.
    pub async fn refresh_inventory_and_automation(&self) -> AppResult<AutomationRunSummary> {
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

        self.run_automation(&inventory, &inventory_policy).await
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
        let limits = AutomationLimits::from_config(self.app_state.config(), inventory_policy);
        let mut summary = AutomationRunSummary {
            inventory_refreshed: true,
            ..AutomationRunSummary::default()
        };

        let rebalance_policy =
            RebalancePolicy::from_config(self.app_state.config(), inventory_policy);
        let automation_state = self.automation_state.read().await.clone();
        match decide_rebalance(
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

        Ok(summary)
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
                self.publish(RuntimeEvent::Gateway(GatewayEvent::Failed {
                    metadata: EventMetadata::new(self.app_state.run_id()),
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
            settlement_status: SettlementStatus::Failed,
            tx_signatures: tx_signatures.to_vec(),
            input_amount: quote.input_amount.clone(),
            output_amount: quote.output_amount.clone(),
        };
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

fn push_signature(signatures: &mut Vec<TxSignature>, signature: Option<&TxSignature>) {
    if let Some(signature) = signature {
        signatures.push(signature.clone());
    }
}
