use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use firmament::assets::AssetRegistry;
use firmament::events::{InventoryEvent, QuoteEvent, RuntimeEvent, SettlementEvent, SwapEvent};
use firmament::ports::{BalanceReader, GatewayClient, HtlcClient, PriceProvider, SwapExecutor};
use firmament::rfq::{RfqRequest, RfqResponse};
use firmament::runtime::{
    RuntimeAdapters, RuntimeOrchestrator, RuntimeOrchestratorOptions, RuntimePersistence,
};
use firmament::types::{
    AmountRaw, AssetId, AssetPair, BalanceSnapshot, GatewayReceipt, GatewayRefillRequest,
    HtlcInitiation, HtlcReceipt, MintAddress, QuoteId, ReferencePrice, RejectionReason,
    SettlementStatus, SwapQuote, SwapReceipt, SwapRequest, TokenAmount, TradeId, TxSignature,
    WalletAddress, WalletRole,
};
use firmament::{AppConfig, AppError, bootstrap};
use rust_decimal::Decimal;
use time::OffsetDateTime;

fn usdc() -> AssetId {
    AssetId::from("USDC")
}

fn sol() -> AssetId {
    AssetId::from("SOL")
}

fn cbbtc() -> AssetId {
    AssetId::from("cbBTC")
}

fn usdc_mint() -> MintAddress {
    MintAddress::new("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
}

fn sol_mint() -> MintAddress {
    MintAddress::new("So11111111111111111111111111111111111111112")
}

fn taker_wallet() -> WalletAddress {
    WalletAddress::new("DemoTaker111111111111111111111111111111111111")
}

fn rfq(amount_raw: u64) -> RfqRequest {
    RfqRequest {
        input_mint: usdc_mint(),
        output_mint: sol_mint(),
        input_amount_raw: AmountRaw::new(amount_raw),
        taker_wallet: taker_wallet(),
        expiry_seconds: Some(30),
    }
}

fn balance_snapshot(balances: Vec<TokenAmount>) -> BalanceSnapshot {
    BalanceSnapshot {
        wallet: WalletRole::Maker,
        balances,
        observed_at: OffsetDateTime::now_utc(),
    }
}

fn quote_inventory() -> BalanceSnapshot {
    balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(10_000_000)),
        TokenAmount::new(sol(), AmountRaw::new(200_000_000)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ])
}

fn post_settlement_drift_inventory() -> BalanceSnapshot {
    balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(1_000_000)),
        TokenAmount::new(sol(), AmountRaw::new(100_000_000)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ])
}

fn funded_demo_inventory() -> BalanceSnapshot {
    balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(3_734_454)),
        TokenAmount::new(sol(), AmountRaw::new(11_611_701)),
        TokenAmount::new(cbbtc(), AmountRaw::new(7_020)),
    ])
}

async fn harness(
    price_provider: FakePriceProvider,
    htlc_client: FakeHtlcClient,
    swap_executor: FakeSwapExecutor,
    gateway_client: FakeGatewayClient,
    balance_reader: FakeBalanceReader,
    options: RuntimeOrchestratorOptions,
) -> RuntimeOrchestrator {
    let app_state = bootstrap(AppConfig::default())
        .await
        .expect("bootstrap runtime state");
    RuntimeOrchestrator::new(
        app_state,
        RuntimeAdapters {
            price_provider: Arc::new(price_provider),
            htlc_client: Arc::new(htlc_client),
            swap_executor: Arc::new(swap_executor),
            gateway_client: Arc::new(gateway_client),
            balance_reader: Arc::new(balance_reader),
        },
        options,
    )
}

async fn persistent_harness(
    price_provider: FakePriceProvider,
    htlc_client: FakeHtlcClient,
    swap_executor: FakeSwapExecutor,
    gateway_client: FakeGatewayClient,
    balance_reader: FakeBalanceReader,
    options: RuntimeOrchestratorOptions,
) -> RuntimeOrchestrator {
    let app_state = bootstrap(AppConfig::default())
        .await
        .expect("bootstrap runtime state");
    RuntimeOrchestrator::new_with_persistence(
        app_state,
        RuntimeAdapters {
            price_provider: Arc::new(price_provider),
            htlc_client: Arc::new(htlc_client),
            swap_executor: Arc::new(swap_executor),
            gateway_client: Arc::new(gateway_client),
            balance_reader: Arc::new(balance_reader),
        },
        Arc::new(
            RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
        ),
        options,
    )
}

#[tokio::test]
async fn runtime_happy_path_orchestrates_rfq_settlement_inventory_and_automation() {
    let htlc = FakeHtlcClient::default();
    let swap = FakeSwapExecutor::default();
    let gateway = FakeGatewayClient::default();
    let balances =
        FakeBalanceReader::new(vec![quote_inventory(), post_settlement_drift_inventory()]);
    let orchestrator = harness(
        FakePriceProvider::default(),
        htlc.clone(),
        swap.clone(),
        gateway.clone(),
        balances,
        RuntimeOrchestratorOptions::default(),
    )
    .await;

    let quote = orchestrator
        .request_rfq(rfq(1_000_000))
        .await
        .expect("request quote");
    let quote_id = match quote {
        RfqResponse::Accepted(quote) => quote.quote_id,
        RfqResponse::Rejected(rejection) => panic!("expected accepted quote: {rejection:?}"),
    };

    let trade = orchestrator
        .accept_quote(quote_id)
        .await
        .expect("accept quote and settle");

    assert_eq!(trade.quote_id, quote_id);
    assert_eq!(trade.settlement_status, SettlementStatus::Redeemed);
    assert_eq!(htlc.initiated_count(), 2);
    assert_eq!(htlc.redeemed_count(), 2);
    assert!(swap.quoted_count() >= 1);
    assert!(swap.executed_count() >= 1);
    assert_eq!(gateway.refill_count(), 1);

    let events = orchestrator.runtime().recent_events(None).await;
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Inventory(InventoryEvent::Snapshot { .. })
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Swap(SwapEvent::Executed { .. })))
    );
    let state = orchestrator.runtime().snapshot().await;
    assert_eq!(state.rfq.accepted_quote_count, 1);
    assert_eq!(state.rfq.active_settlement_count, 0);
    assert!(state.rebalance.completed_swap_count >= 1);
    assert_eq!(state.gateway.status, "refill_completed");
}

#[tokio::test]
async fn runtime_fake_adapter_flow_feeds_balanced_ledger_and_pnl_projection() {
    let orchestrator = persistent_harness(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory(), post_settlement_drift_inventory()]),
        RuntimeOrchestratorOptions::default(),
    )
    .await;

    let quote_id = accepted_quote_id(orchestrator.request_rfq(rfq(1_000_000)).await);
    orchestrator
        .accept_quote(quote_id)
        .await
        .expect("fake-adapter settlement succeeds");

    let summary = orchestrator
        .ledger_summary()
        .expect("automatic ledger summary");
    assert!(summary.balanced);
    assert!(summary.entry_count > 0);
    assert!(summary.net_usdc_estimate < Decimal::ZERO);
    let state = orchestrator.runtime().snapshot().await;
    assert!(state.pnl.rebalance_cost_usdc_estimate < Decimal::ZERO);
}

#[tokio::test]
async fn runtime_rfq_rejection_stops_before_settlement() {
    let htlc = FakeHtlcClient::default();
    let orchestrator = harness(
        FakePriceProvider::default(),
        htlc.clone(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory()]),
        RuntimeOrchestratorOptions::default(),
    )
    .await;

    let quote = orchestrator
        .request_rfq(rfq(3_000_000))
        .await
        .expect("request rejected quote");
    let quote_id = match quote {
        RfqResponse::Rejected(rejection) => {
            assert_eq!(rejection.reason, RejectionReason::MaxNotionalExceeded);
            rejection.quote_id.expect("rejected quote id")
        }
        RfqResponse::Accepted(quote) => panic!("expected rejection: {quote:?}"),
    };

    let error = orchestrator
        .accept_quote(quote_id)
        .await
        .expect_err("rejected quote is not stored for settlement");

    assert!(error.to_string().contains("unknown or rejected quote"));
    assert_eq!(htlc.initiated_count(), 0);
    assert_eq!(
        orchestrator
            .runtime()
            .snapshot()
            .await
            .rfq
            .rejected_quote_count,
        1
    );
}

#[tokio::test]
async fn runtime_maker_only_mode_rejects_local_taker_settlement_before_htlc() {
    let htlc = FakeHtlcClient::default();
    let orchestrator = harness(
        FakePriceProvider::default(),
        htlc.clone(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory()]),
        RuntimeOrchestratorOptions {
            allow_local_taker_settlement: false,
            ..RuntimeOrchestratorOptions::default()
        },
    )
    .await;

    let quote_id = accepted_quote_id(orchestrator.request_rfq(rfq(1_000_000)).await);
    let error = orchestrator
        .accept_quote(quote_id)
        .await
        .expect_err("maker-only runtime cannot run local two-wallet settlement");

    assert!(error.to_string().contains("wallet settlement endpoints"));
    assert_eq!(htlc.initiated_count(), 0);
}

#[tokio::test]
async fn runtime_settlement_failure_emits_failure_and_skips_rebalance() {
    let htlc = FakeHtlcClient::with_failure(HtlcFailure::MakerInitiate);
    let swap = FakeSwapExecutor::default();
    let orchestrator = harness(
        FakePriceProvider::default(),
        htlc,
        swap.clone(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory(), post_settlement_drift_inventory()]),
        RuntimeOrchestratorOptions::default(),
    )
    .await;

    let quote_id = accepted_quote_id(orchestrator.request_rfq(rfq(1_000_000)).await);
    let error = orchestrator
        .accept_quote(quote_id)
        .await
        .expect_err("maker HTLC failure should fail settlement");

    assert!(error.to_string().contains("maker initiate failed"));
    assert_eq!(swap.quoted_count(), 0);
    let events = orchestrator.runtime().recent_events(None).await;
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Settlement(SettlementEvent::Failed { .. })
    )));
}

#[tokio::test]
async fn runtime_post_settlement_inventory_event_triggers_rebalance_decision() {
    let swap = FakeSwapExecutor::default();
    let orchestrator = harness(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        swap.clone(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory(), post_settlement_drift_inventory()]),
        RuntimeOrchestratorOptions::default(),
    )
    .await;

    let quote_id = accepted_quote_id(orchestrator.request_rfq(rfq(1_000_000)).await);
    orchestrator
        .accept_quote(quote_id)
        .await
        .expect("settlement succeeds");

    assert!(
        swap.quoted_requests()
            .iter()
            .any(|request| request.pair == AssetPair::new(sol(), usdc()))
    );
    let events = orchestrator.runtime().recent_events(None).await;
    let snapshot_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                RuntimeEvent::Inventory(InventoryEvent::Snapshot { .. })
            )
        })
        .expect("inventory snapshot event");
    let swap_index = events
        .iter()
        .position(|event| matches!(event, RuntimeEvent::Swap(SwapEvent::Quoted { .. })))
        .expect("swap quote event");
    assert!(snapshot_index < swap_index);
}

#[tokio::test]
async fn runtime_cap_reached_blocks_post_settlement_automation() {
    let swap = FakeSwapExecutor::default();
    let gateway = FakeGatewayClient::default();
    let orchestrator = harness(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        swap.clone(),
        gateway.clone(),
        FakeBalanceReader::new(vec![quote_inventory(), post_settlement_drift_inventory()]),
        RuntimeOrchestratorOptions {
            initial_automation_spend_usd: Decimal::from(15),
            ..RuntimeOrchestratorOptions::default()
        },
    )
    .await;

    let quote_id = accepted_quote_id(orchestrator.request_rfq(rfq(1_000_000)).await);
    orchestrator
        .accept_quote(quote_id)
        .await
        .expect("settlement succeeds");

    assert_eq!(swap.quoted_count(), 0);
    assert_eq!(gateway.refill_count(), 0);
    let events = orchestrator.runtime().recent_events(None).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Swap(SwapEvent::Failed { reason, .. }) if reason.contains("cumulative cap")))
    );
}

#[tokio::test]
async fn web_app_replacement_calls_runtime_orchestrator_directly() {
    let orchestrator = harness(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory(), post_settlement_drift_inventory()]),
        RuntimeOrchestratorOptions {
            demo_taker_wallet: Some(taker_wallet()),
            ..RuntimeOrchestratorOptions::default()
        },
    )
    .await;

    // The web app operator flow reaches the same RuntimeOrchestrator commands.
    orchestrator
        .request_operator_rfq(&usdc(), &sol(), AmountRaw::new(1_000_000))
        .await
        .expect("generate RFQ");
    orchestrator
        .accept_latest_quote()
        .await
        .expect("accept RFQ");

    let state = orchestrator.runtime().snapshot().await;
    assert_eq!(state.rfq.accepted_quote_count, 1);
    assert_eq!(state.rfq.active_settlement_count, 0);
}

#[tokio::test]
async fn web_tiny_demo_rfq_fits_funded_mainnet_wallet_gas_reserve() {
    let orchestrator = harness(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![funded_demo_inventory()]),
        RuntimeOrchestratorOptions {
            demo_taker_wallet: Some(taker_wallet()),
            ..RuntimeOrchestratorOptions::default()
        },
    )
    .await;

    let response = orchestrator
        .generate_tiny_demo_rfq()
        .await
        .expect("generate tiny RFQ");

    match response {
        RfqResponse::Accepted(quote) => {
            assert_eq!(quote.input_amount.amount_raw, AmountRaw::new(100_000));
        }
        RfqResponse::Rejected(rejection) => {
            panic!("tiny funded-wallet demo RFQ should be accepted: {rejection:?}");
        }
    }
}

#[tokio::test]
async fn web_oversized_demo_rfq_shows_inventory_threshold_rejection() {
    let orchestrator = harness(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![funded_demo_inventory()]),
        RuntimeOrchestratorOptions {
            demo_taker_wallet: Some(taker_wallet()),
            ..RuntimeOrchestratorOptions::default()
        },
    )
    .await;

    let response = orchestrator
        .generate_oversized_demo_rfq()
        .await
        .expect("generate oversized RFQ");

    match response {
        RfqResponse::Rejected(rejection) => {
            assert_eq!(
                rejection.reason,
                RejectionReason::InventoryBelowQuoteableThreshold
            );
        }
        RfqResponse::Accepted(quote) => {
            panic!("oversized funded-wallet demo RFQ should be rejected: {quote:?}");
        }
    }
}

#[tokio::test]
async fn runtime_state_projection_receives_events_in_order() {
    let app_state = bootstrap(AppConfig::default())
        .await
        .expect("bootstrap runtime state");
    let runtime = app_state.runtime();
    let run_id = app_state.run_id();
    let quote_id = QuoteId::generate();
    let trade_id = TradeId::generate();

    runtime
        .publish_event(RuntimeEvent::Quote(QuoteEvent::Requested {
            metadata: firmament::events::EventMetadata::new(run_id),
            quote_id,
            pair: AssetPair::new(usdc(), sol()),
            input_amount: TokenAmount::new(usdc(), AmountRaw::new(1_000_000)),
            taker_wallet: taker_wallet(),
            expires_at: OffsetDateTime::now_utc() + time::Duration::seconds(30),
        }))
        .await
        .expect("publish requested");
    runtime
        .publish_event(RuntimeEvent::Quote(QuoteEvent::Accepted {
            metadata: firmament::events::EventMetadata::new(run_id),
            quote_id,
            trade_id,
        }))
        .await
        .expect("publish accepted");
    runtime
        .publish_event(RuntimeEvent::Settlement(SettlementEvent::Started {
            metadata: firmament::events::EventMetadata::new(run_id),
            trade_id,
            quote_id,
        }))
        .await
        .expect("publish settlement");

    let events = runtime.recent_events(None).await;
    let last_three = events
        .iter()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    assert!(matches!(
        last_three[0],
        RuntimeEvent::Quote(QuoteEvent::Requested { .. })
    ));
    assert!(matches!(
        last_three[1],
        RuntimeEvent::Quote(QuoteEvent::Accepted { .. })
    ));
    assert!(matches!(
        last_three[2],
        RuntimeEvent::Settlement(SettlementEvent::Started { .. })
    ));

    let state = runtime.snapshot().await;
    assert_eq!(state.rfq.accepted_quote_count, 1);
    assert_eq!(state.rfq.active_settlement_count, 1);
}

fn accepted_quote_id(result: Result<RfqResponse, AppError>) -> QuoteId {
    match result.expect("request quote") {
        RfqResponse::Accepted(quote) => quote.quote_id,
        RfqResponse::Rejected(rejection) => panic!("expected accepted quote: {rejection:?}"),
    }
}

#[derive(Debug, Clone)]
struct FakePriceProvider {
    prices: Arc<HashMap<AssetPair, Decimal>>,
}

impl Default for FakePriceProvider {
    fn default() -> Self {
        Self {
            prices: Arc::new(HashMap::from([
                (AssetPair::new(usdc(), sol()), Decimal::new(1, 2)),
                (AssetPair::new(sol(), usdc()), Decimal::from(100)),
            ])),
        }
    }
}

#[async_trait]
impl PriceProvider for FakePriceProvider {
    async fn reference_price(&self, pair: AssetPair) -> Result<ReferencePrice, AppError> {
        let price = self
            .prices
            .get(&pair)
            .copied()
            .ok_or_else(|| AppError::unsupported(format!("fake price missing for {pair:?}")))?;
        Ok(ReferencePrice {
            pair,
            output_per_input: price,
            observed_at: OffsetDateTime::now_utc(),
        })
    }
}

#[derive(Debug, Clone, Default)]
struct FakeHtlcClient {
    inner: Arc<Mutex<FakeHtlcState>>,
}

#[derive(Debug, Default)]
struct FakeHtlcState {
    initiated: Vec<HtlcInitiation>,
    redeemed: Vec<(TradeId, String)>,
    failure: Option<HtlcFailure>,
}

#[derive(Debug, Clone, Copy)]
enum HtlcFailure {
    MakerInitiate,
}

impl FakeHtlcClient {
    fn with_failure(failure: HtlcFailure) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeHtlcState {
                failure: Some(failure),
                ..FakeHtlcState::default()
            })),
        }
    }

    fn initiated_count(&self) -> usize {
        self.inner.lock().expect("htlc lock").initiated.len()
    }

    fn redeemed_count(&self) -> usize {
        self.inner.lock().expect("htlc lock").redeemed.len()
    }
}

#[async_trait]
impl HtlcClient for FakeHtlcClient {
    async fn initiate(&self, request: HtlcInitiation) -> Result<HtlcReceipt, AppError> {
        let mut state = self.inner.lock().expect("htlc lock");
        if matches!(state.failure, Some(HtlcFailure::MakerInitiate)) && state.initiated.len() == 1 {
            return Err(AppError::solana("maker initiate failed"));
        }
        state.initiated.push(request.clone());
        Ok(HtlcReceipt {
            trade_id: request.trade_id,
            status: SettlementStatus::Initiated,
            signature: Some(TxSignature::new(format!("init-{}", state.initiated.len()))),
        })
    }

    async fn redeem(&self, trade_id: TradeId, preimage: String) -> Result<HtlcReceipt, AppError> {
        let mut state = self.inner.lock().expect("htlc lock");
        state.redeemed.push((trade_id, preimage));
        Ok(HtlcReceipt {
            trade_id,
            status: SettlementStatus::Redeemed,
            signature: Some(TxSignature::new(format!("redeem-{}", state.redeemed.len()))),
        })
    }

    async fn refund(&self, trade_id: TradeId) -> Result<HtlcReceipt, AppError> {
        Ok(HtlcReceipt {
            trade_id,
            status: SettlementStatus::Refunded,
            signature: Some(TxSignature::new("refund")),
        })
    }

    async fn status(&self, _trade_id: TradeId) -> Result<SettlementStatus, AppError> {
        Ok(SettlementStatus::Initiated)
    }
}

#[derive(Debug, Clone, Default)]
struct FakeSwapExecutor {
    quoted: Arc<Mutex<Vec<SwapRequest>>>,
    executed: Arc<Mutex<Vec<SwapQuote>>>,
}

impl FakeSwapExecutor {
    fn quoted_count(&self) -> usize {
        self.quoted.lock().expect("swap quote lock").len()
    }

    fn executed_count(&self) -> usize {
        self.executed.lock().expect("swap execute lock").len()
    }

    fn quoted_requests(&self) -> Vec<SwapRequest> {
        self.quoted.lock().expect("swap quote lock").clone()
    }
}

#[async_trait]
impl SwapExecutor for FakeSwapExecutor {
    async fn quote_swap(&self, request: SwapRequest) -> Result<SwapQuote, AppError> {
        self.quoted
            .lock()
            .expect("swap quote lock")
            .push(request.clone());
        Ok(SwapQuote {
            expected_output: TokenAmount::new(request.pair.output.clone(), AmountRaw::new(10)),
            estimated_fee: Some(TokenAmount::new(usdc(), AmountRaw::new(10_000))),
            expires_at: None,
            request,
        })
    }

    async fn execute_swap(&self, quote: SwapQuote) -> Result<SwapReceipt, AppError> {
        self.executed
            .lock()
            .expect("swap execute lock")
            .push(quote.clone());
        Ok(SwapReceipt {
            trade_id: None,
            signature: TxSignature::new(format!(
                "swap-{}",
                self.executed.lock().expect("swap execute lock").len()
            )),
            output_amount: Some(quote.expected_output),
        })
    }
}

#[derive(Debug, Clone, Default)]
struct FakeGatewayClient {
    refills: Arc<Mutex<Vec<GatewayRefillRequest>>>,
}

impl FakeGatewayClient {
    fn refill_count(&self) -> usize {
        self.refills.lock().expect("gateway lock").len()
    }
}

#[async_trait]
impl GatewayClient for FakeGatewayClient {
    async fn balance(&self, asset: AssetId) -> Result<GatewayReceipt, AppError> {
        Ok(GatewayReceipt {
            amount: TokenAmount::new(asset, AmountRaw::new(10_000_000)),
            provider_transfer_id: None,
            signature: None,
        })
    }

    async fn request_refill(
        &self,
        request: GatewayRefillRequest,
    ) -> Result<GatewayReceipt, AppError> {
        self.refills
            .lock()
            .expect("gateway lock")
            .push(request.clone());
        Ok(GatewayReceipt {
            amount: request.amount,
            provider_transfer_id: Some("gateway-transfer".to_owned()),
            signature: Some(TxSignature::new("gateway-sig")),
        })
    }
}

#[derive(Debug, Clone)]
struct FakeBalanceReader {
    snapshots: Arc<Mutex<Vec<BalanceSnapshot>>>,
}

impl FakeBalanceReader {
    fn new(snapshots: Vec<BalanceSnapshot>) -> Self {
        Self {
            snapshots: Arc::new(Mutex::new(snapshots.into_iter().rev().collect())),
        }
    }
}

#[async_trait]
impl BalanceReader for FakeBalanceReader {
    async fn balances(&self, _wallet: WalletRole) -> Result<BalanceSnapshot, AppError> {
        let mut snapshots = self.snapshots.lock().expect("balance lock");
        if snapshots.len() == 1 {
            return Ok(snapshots[0].clone());
        }
        snapshots
            .pop()
            .ok_or_else(|| AppError::internal("fake balance reader has no snapshots"))
    }
}
