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
use crate::application::hedge::{HedgeDecision, HedgePolicy, decide_hedge};
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
    AmountRaw, AssetId, BalanceSnapshot, QuoteId, SettlementStatus, TokenAmount, TradeId,
    TxSignature, WalletRole,
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
    /// Jupiter-backed swap executor used by rebalance and hedge decisions.
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
    /// Taker wallet address used by scripted TUI demo RFQs.
    pub demo_taker_wallet: Option<crate::domain::types::WalletAddress>,
}

impl Default for RuntimeOrchestratorOptions {
    fn default() -> Self {
        Self {
            initial_automation_spend_usd: Decimal::ZERO,
            demo_taker_wallet: None,
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
            hedge_cost_usdc_estimate: summary.hedge_cost_usdc,
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
    automation_state: Arc<RwLock<AutomationState>>,
    usd_prices: Arc<RwLock<BTreeMap<AssetId, Decimal>>>,
    persistence: Option<Arc<RuntimePersistence>>,
    last_accepted_quote: Arc<RwLock<Option<QuoteId>>>,
    demo_taker_wallet: Option<crate::domain::types::WalletAddress>,
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
            automation_state: Arc::new(RwLock::new(automation_state)),
            usd_prices: Arc::new(RwLock::new(usd_prices)),
            persistence: None,
            last_accepted_quote: Arc::new(RwLock::new(None)),
            demo_taker_wallet: options.demo_taker_wallet,
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

    /// Refresh working inventory and run rebalance, hedge, and Gateway refill
    /// decisions from the resulting snapshot.
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

    /// Generate the scripted normal tiny USDC->SOL RFQ used by the TUI demo.
    ///
    /// # Errors
    ///
    /// Returns adapter, quote, risk, or event persistence errors from RFQ
    /// handling.
    pub async fn generate_tiny_demo_rfq(&self) -> AppResult<RfqResponse> {
        self.request_rfq(self.demo_rfq(AmountRaw::new(1_000_000))?)
            .await
    }

    /// Generate the scripted oversized RFQ used to demonstrate risk rejection.
    ///
    /// # Errors
    ///
    /// Returns adapter, quote, risk, or event persistence errors from RFQ
    /// handling.
    pub async fn generate_oversized_demo_rfq(&self) -> AppResult<RfqResponse> {
        self.request_rfq(self.demo_rfq(AmountRaw::new(3_000_000))?)
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

        let hedge_policy = HedgePolicy::from_config(self.app_state.config());
        let automation_state = self.automation_state.read().await.clone();
        match decide_hedge(
            inventory,
            &automation_state,
            &limits,
            &hedge_policy,
            OffsetDateTime::now_utc(),
        ) {
            HedgeDecision::Swap(plan) => {
                if self.submit_swap_plan(plan, &limits).await? {
                    summary.completed_swaps += 1;
                }
            }
            HedgeDecision::NoAction { reason } => {
                summary.blocked_reasons.push(reason);
                self.publish_cap_block_if_needed("hedge", reason).await?;
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
                    plan.uses_cbbtc_exception,
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
        uses_cbbtc_exception: bool,
    ) {
        let mut state = self.automation_state.write().await;
        state.cumulative_spend_usd += notional_usd;
        state
            .last_submitted_at
            .insert(key.clone(), OffsetDateTime::now_utc());
        if uses_cbbtc_exception {
            state.cbbtc_exception_used = true;
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
        let input_mint = self.mint_for_asset("USDC")?;
        let output_mint = self.mint_for_asset("SOL")?;
        let taker_wallet = self.demo_taker_wallet.clone().ok_or_else(|| {
            AppError::config("TUI demo commands require a configured taker wallet address")
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

    fn mint_for_asset(&self, id: &str) -> AppResult<crate::domain::types::MintAddress> {
        self.app_state
            .config()
            .assets
            .supported
            .iter()
            .find(|asset| asset.id.as_str() == id)
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
