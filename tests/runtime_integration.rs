use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use firmament::assets::AssetRegistry;
use firmament::db::Db;
use firmament::events::{
    AutomationEvent, AutomationKind, AutomationOutcome, EventMetadata, GatewayEvent,
    InventoryEvent, QuoteEvent, RuntimeEvent, SettlementEvent, SwapEvent,
};
use firmament::ledger::{LedgerAccountId, LedgerEventConsumer, SqliteLedgerRepository};
use firmament::ports::{BalanceReader, GatewayClient, HtlcClient, PriceProvider, SwapExecutor};
use firmament::rfq::{RfqRequest, RfqResponse};
use firmament::runtime::{
    RuntimeAdapters, RuntimeOrchestrator, RuntimeOrchestratorOptions, RuntimePersistence,
    TradeSignatureKind,
};
use firmament::settlement::SettlementLeg;
use firmament::types::{
    AmountRaw, AssetId, AssetPair, BalanceSnapshot, ExecutionPath, GatewayReceipt,
    GatewayRefillRequest, HtlcInitiation, HtlcReceipt, MintAddress, QuoteId, ReferencePrice,
    RejectionReason, SettlementStatus, SwapQuote, SwapReceipt, SwapRequest, TokenAmount, TradeId,
    TxSignature, WalletAddress, WalletRole,
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

/// Relax per-asset min/max notional caps on every supported asset to a wide
/// test range so legacy tests that send arbitrary mocked amounts continue to
/// reach the gate they are exercising. The default config caps trades at $1-$2
/// (or $1-$5 for cbBTC), which the per-asset gate (correctly) enforces.
fn relax_asset_notional_limits(config: &mut AppConfig) {
    for asset in &mut config.assets.supported {
        asset.min_trade_notional_usd = Decimal::new(1, 6); // $0.000001
        asset.max_trade_notional_usd = Decimal::from(1_000);
    }
}

fn relaxed_default_config() -> AppConfig {
    let mut config = AppConfig::default();
    relax_asset_notional_limits(&mut config);
    config
}

async fn harness(
    price_provider: FakePriceProvider,
    htlc_client: FakeHtlcClient,
    swap_executor: FakeSwapExecutor,
    gateway_client: FakeGatewayClient,
    balance_reader: FakeBalanceReader,
    options: RuntimeOrchestratorOptions,
) -> RuntimeOrchestrator {
    let app_state = bootstrap(relaxed_default_config())
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

async fn persistent_harness_with_persistence(
    price_provider: FakePriceProvider,
    htlc_client: FakeHtlcClient,
    swap_executor: FakeSwapExecutor,
    gateway_client: FakeGatewayClient,
    balance_reader: FakeBalanceReader,
    options: RuntimeOrchestratorOptions,
    persistence: Arc<RuntimePersistence>,
) -> RuntimeOrchestrator {
    persistent_harness_with_config(
        relaxed_default_config(),
        price_provider,
        htlc_client,
        swap_executor,
        gateway_client,
        balance_reader,
        options,
        persistence,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn persistent_harness_with_config(
    config: AppConfig,
    price_provider: FakePriceProvider,
    htlc_client: FakeHtlcClient,
    swap_executor: FakeSwapExecutor,
    gateway_client: FakeGatewayClient,
    balance_reader: FakeBalanceReader,
    options: RuntimeOrchestratorOptions,
    persistence: Arc<RuntimePersistence>,
) -> RuntimeOrchestrator {
    let app_state = bootstrap(config).await.expect("bootstrap runtime state");
    RuntimeOrchestrator::new_with_persistence(
        app_state,
        RuntimeAdapters {
            price_provider: Arc::new(price_provider),
            htlc_client: Arc::new(htlc_client),
            swap_executor: Arc::new(swap_executor),
            gateway_client: Arc::new(gateway_client),
            balance_reader: Arc::new(balance_reader),
        },
        persistence,
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
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    // RFQ inventory now reads from the ledger's working_custody. Seed the
    // ledger to match the live wallet snapshot so this end-to-end test still
    // observes the same accept/settle path.
    seed_working_custody(&persistence, &sol(), 200_000_000);
    seed_working_custody(&persistence, &usdc(), 10_000_000);

    let orchestrator = persistent_harness_with_persistence(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory(), post_settlement_drift_inventory()]),
        RuntimeOrchestratorOptions::default(),
        persistence,
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
async fn runtime_request_rfq_expires_stored_quotes_before_new_quote() {
    let orchestrator = harness(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory(), quote_inventory()]),
        RuntimeOrchestratorOptions::default(),
    )
    .await;

    let mut expiring_rfq = rfq(1_000);
    expiring_rfq.expiry_seconds = Some(1);
    let first_quote = accepted_quote_id(orchestrator.request_rfq(expiring_rfq).await);
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let second_quote = accepted_quote_id(orchestrator.request_rfq(rfq(1_000_000)).await);

    assert_ne!(first_quote, second_quote);
    let state = orchestrator.runtime().snapshot().await;
    assert_eq!(state.rfq.active_quote_count, 1);
    let events = orchestrator.runtime().recent_events(None).await;
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Quote(QuoteEvent::Expired { quote_id, .. }) if *quote_id == first_quote
    )));
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

fn seed_working_custody(persistence: &Arc<RuntimePersistence>, asset: &AssetId, amount: u64) {
    if amount == 0 {
        return;
    }
    let transaction = firmament::ledger::LedgerTransactionBuilder::new(
        "test_seed_working_custody",
        uuid::Uuid::now_v7(),
    )
    .description("seed working custody for RFQ inventory test")
    .idempotency_key(format!(
        "test:seed:working:{}:{}:{}",
        asset.as_str(),
        amount,
        uuid::Uuid::now_v7(),
    ))
    .debit(
        LedgerAccountId::working(asset.clone()),
        AmountRaw::new(amount),
    )
    .credit(
        LedgerAccountId::external(asset.clone(), "seed"),
        AmountRaw::new(amount),
    )
    .build()
    .expect("balanced seed transaction");
    persistence
        .save_ledger_transaction(&transaction)
        .expect("save seed ledger transaction");
}

fn drain_working_custody(persistence: &Arc<RuntimePersistence>, asset: &AssetId, amount: u64) {
    if amount == 0 {
        return;
    }
    let transaction = firmament::ledger::LedgerTransactionBuilder::new(
        "test_drain_working_custody",
        uuid::Uuid::now_v7(),
    )
    .description("drain working custody for RFQ inventory test")
    .idempotency_key(format!(
        "test:drain:working:{}:{}:{}",
        asset.as_str(),
        amount,
        uuid::Uuid::now_v7(),
    ))
    .debit(
        LedgerAccountId::external(asset.clone(), "drain"),
        AmountRaw::new(amount),
    )
    .credit(
        LedgerAccountId::working(asset.clone()),
        AmountRaw::new(amount),
    )
    .build()
    .expect("balanced drain transaction");
    persistence
        .save_ledger_transaction(&transaction)
        .expect("save drain ledger transaction");
}

fn seed_gateway_balance(persistence: &Arc<RuntimePersistence>, asset: &AssetId, amount: u64) {
    if amount == 0 {
        return;
    }
    let transaction =
        firmament::ledger::LedgerTransactionBuilder::new("test_seed_gateway", uuid::Uuid::now_v7())
            .description("seed gateway USDC for Gateway-quoteability test")
            .idempotency_key(format!(
                "test:seed:gateway:{}:{}:{}",
                asset.as_str(),
                amount,
                uuid::Uuid::now_v7(),
            ))
            .debit(
                LedgerAccountId::gateway(asset.clone()),
                AmountRaw::new(amount),
            )
            .credit(
                LedgerAccountId::external(asset.clone(), "seed_gateway"),
                AmountRaw::new(amount),
            )
            .build()
            .expect("balanced gateway seed transaction");
    persistence
        .save_ledger_transaction(&transaction)
        .expect("save gateway seed ledger transaction");
}

fn seed_gateway_reserved(
    persistence: &Arc<RuntimePersistence>,
    asset: &AssetId,
    trade_id: &str,
    amount: u64,
) {
    if amount == 0 {
        return;
    }
    // Seed gateway_reserved against an external bookkeeping account so the
    // gateway account itself stays at its full seeded size. The T9 gate is
    // `gateway − sum(gateway_reserved:*)`, which depends on both legs being
    // observable independently in the ledger.
    let transaction = firmament::ledger::LedgerTransactionBuilder::new(
        "test_seed_gateway_reserved",
        uuid::Uuid::now_v7(),
    )
    .description("seed gateway_reserved USDC for in-flight trade")
    .idempotency_key(format!(
        "test:seed:gateway_reserved:{}:{}:{}:{}",
        asset.as_str(),
        trade_id,
        amount,
        uuid::Uuid::now_v7(),
    ))
    .debit(
        LedgerAccountId::gateway_reserved(asset.clone(), trade_id.to_owned()),
        AmountRaw::new(amount),
    )
    .credit(
        LedgerAccountId::external(asset.clone(), "seed_gateway_reserved"),
        AmountRaw::new(amount),
    )
    .build()
    .expect("balanced gateway_reserved seed transaction");
    persistence
        .save_ledger_transaction(&transaction)
        .expect("save gateway_reserved seed ledger transaction");
}

#[tokio::test]
async fn rfq_quoteability_uses_ledger_working_custody_not_live_balance() {
    // Live wallet reader returns ZERO SOL — if the gate used the live
    // snapshot, every quote would reject for inventory. The ledger working
    // custody is the new operational source of truth.
    let zero_sol_inventory = balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(10_000_000)),
        TokenAmount::new(sol(), AmountRaw::new(0)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ]);

    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    // Seed ledger working_custody with SOL well above quoteable threshold +
    // gas buffer so the RFQ should be accepted purely on the ledger view.
    seed_working_custody(&persistence, &sol(), 200_000_000);
    // USDC is the input asset — having some on the ledger keeps the snapshot
    // truthful but the gate is on the output asset.
    seed_working_custody(&persistence, &usdc(), 10_000_000);

    let orchestrator = persistent_harness_with_persistence(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![zero_sol_inventory.clone(), zero_sol_inventory]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    // Scenario A: ledger has SOL, live wallet reports zero — should ACCEPT.
    let response = orchestrator
        .request_rfq(rfq(100_000))
        .await
        .expect("request quote");
    assert!(
        matches!(response, RfqResponse::Accepted(_)),
        "expected Accepted, got {response:?}"
    );

    // Scenario B: drain the ledger working_custody to zero. Live wallet
    // reader still has its same configuration (which would have looked
    // healthy if it were authoritative), but the ledger now says no SOL.
    drain_working_custody(&persistence, &sol(), 200_000_000);

    let response = orchestrator
        .request_rfq(rfq(100_000))
        .await
        .expect("request quote after drain");
    match response {
        RfqResponse::Rejected(rejection) => {
            assert_eq!(
                rejection.reason,
                RejectionReason::InventoryBelowQuoteableThreshold,
                "expected InventoryBelowQuoteableThreshold, got {:?}",
                rejection.reason
            );
        }
        RfqResponse::Accepted(quote) => {
            panic!(
                "expected rejection once ledger working_custody was drained, got Accepted: {quote:?}"
            )
        }
    }
}

#[tokio::test]
async fn rfq_gateway_quoteability_uses_ledger_minus_reserved() {
    // T9: when working_custody is insufficient for the requested output, the
    // RFQ gate must consider the Gateway-backed path. The Gateway gate uses
    // FREE Gateway USDC = gateway − sum(gateway_reserved:USDC:*). With
    // SOL=$200 reference price:
    //   - gateway:USDC = 1_000_000_000 (1000 USDC)
    //   - gateway_reserved:USDC[trade-A] = 400_000_000 (400 USDC)
    //   - free Gateway USDC = 600 USDC → covers up to 3.0 SOL output
    //
    // RFQ for 500 USDC input (2.5 SOL output) → Accepted via Gateway path.
    // RFQ for 700 USDC input (3.5 SOL output) → Rejected (free Gateway short).
    //
    // The default risk limits cap notional at $2/quote so this test bumps
    // them to $1000 to make the gate the load-bearing assertion. The
    // per-asset gate is also relaxed for the same reason.
    let mut config = AppConfig::default();
    config.risk.max_quote_notional_usd = Decimal::from(1_000);
    config.risk.max_trade_notional_usd = Decimal::from(1_000);
    config.risk.max_daily_notional_usd = Decimal::from(10_000);
    config.risk.max_non_stable_asset_notional_usd = Decimal::from(1_000);
    relax_asset_notional_limits(&mut config);

    // SOL = $200 reference: 1 USDC = 0.005 SOL, 1 SOL = 200 USDC.
    let price_provider = FakePriceProvider {
        prices: Arc::new(HashMap::from([
            (AssetPair::new(usdc(), sol()), Decimal::new(5, 3)),
            (AssetPair::new(sol(), usdc()), Decimal::from(200)),
        ])),
    };

    let zero_sol_inventory = balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(0)),
        TokenAmount::new(sol(), AmountRaw::new(0)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ]);

    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    // working_custody:SOL = 0 (force Gateway path). USDC seeded so the input
    // asset still appears in the ledger snapshot.
    seed_working_custody(&persistence, &usdc(), 100_000_000);
    seed_gateway_balance(&persistence, &usdc(), 1_000_000_000);
    seed_gateway_reserved(&persistence, &usdc(), "trade-A", 400_000_000);

    let orchestrator = persistent_harness_with_config(
        config,
        price_provider,
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![
            zero_sol_inventory.clone(),
            zero_sol_inventory.clone(),
            zero_sol_inventory,
        ]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    // Scenario A: 500 USDC → 2.5 SOL output ≤ 3.0 SOL synthetic Gateway
    // supply → ACCEPT.
    let response = orchestrator
        .request_rfq(rfq(500_000_000))
        .await
        .expect("request 500 USDC quote");
    assert!(
        matches!(response, RfqResponse::Accepted(_)),
        "expected Accepted via Gateway path (500 USDC < 600 free), got {response:?}"
    );

    // Scenario B: 700 USDC → 3.5 SOL output > 3.0 SOL synthetic Gateway
    // supply → REJECT for inventory.
    let response = orchestrator
        .request_rfq(rfq(700_000_000))
        .await
        .expect("request 700 USDC quote");
    match response {
        RfqResponse::Rejected(rejection) => {
            assert_eq!(
                rejection.reason,
                RejectionReason::InventoryBelowQuoteableThreshold,
                "expected InventoryBelowQuoteableThreshold (700 USDC > 600 free), got {:?}",
                rejection.reason
            );
        }
        RfqResponse::Accepted(quote) => {
            panic!("expected Gateway-path rejection at 700 USDC, got Accepted: {quote:?}")
        }
    }
}

#[tokio::test]
async fn quote_records_inventory_to_inventory_path_when_custody_covers() {
    // working_custody:SOL covers a small RFQ output → quote should resolve to
    // ExecutionPath::InventoryToInventory.
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &sol(), 200_000_000);
    seed_working_custody(&persistence, &usdc(), 10_000_000);

    let orchestrator = persistent_harness_with_persistence(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory(), quote_inventory()]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq(100_000))
        .await
        .expect("request quote");
    let quote = match response {
        RfqResponse::Accepted(quote) => quote,
        RfqResponse::Rejected(rejection) => panic!("expected accepted: {rejection:?}"),
    };
    assert_eq!(
        quote.execution_path,
        ExecutionPath::InventoryToInventory,
        "expected InventoryToInventory when working_custody covers, got {:?}",
        quote.execution_path,
    );
}

#[tokio::test]
async fn quote_records_gateway_to_dex_path_when_only_gateway_covers() {
    // working_custody:SOL = 0; gateway:USDC seeded with enough free supply to
    // cover the requested SOL output. Quote should resolve to
    // ExecutionPath::GatewayToDex. Per-asset notional caps are relaxed so the
    // Gateway-path resolution is the load-bearing assertion.
    let mut config = AppConfig::default();
    config.risk.max_quote_notional_usd = Decimal::from(1_000);
    config.risk.max_trade_notional_usd = Decimal::from(1_000);
    config.risk.max_daily_notional_usd = Decimal::from(10_000);
    config.risk.max_non_stable_asset_notional_usd = Decimal::from(1_000);
    relax_asset_notional_limits(&mut config);

    // SOL = $200 reference: 1 USDC = 0.005 SOL.
    let price_provider = FakePriceProvider {
        prices: Arc::new(HashMap::from([
            (AssetPair::new(usdc(), sol()), Decimal::new(5, 3)),
            (AssetPair::new(sol(), usdc()), Decimal::from(200)),
        ])),
    };

    let zero_sol_inventory = balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(0)),
        TokenAmount::new(sol(), AmountRaw::new(0)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ]);

    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    // working_custody:SOL = 0; USDC seeded enough for the request input balance
    // and for the quoteable threshold to consider the input asset present.
    seed_working_custody(&persistence, &usdc(), 1_000_000_000);
    // Free Gateway USDC = 1_000_000_000 (= 1000 USDC = 5 SOL synthetic supply).
    seed_gateway_balance(&persistence, &usdc(), 1_000_000_000);

    let orchestrator = persistent_harness_with_config(
        config,
        price_provider,
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![zero_sol_inventory.clone(), zero_sol_inventory]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    // Request 200 USDC input → 1.0 SOL output (well within the 5 SOL synthetic
    // supply). Working custody for SOL is empty so this must select the
    // Gateway path.
    let response = orchestrator
        .request_rfq(rfq(200_000_000))
        .await
        .expect("request quote");
    let quote = match response {
        RfqResponse::Accepted(quote) => quote,
        RfqResponse::Rejected(rejection) => panic!("expected accepted: {rejection:?}"),
    };
    assert_eq!(
        quote.execution_path,
        ExecutionPath::GatewayToDex,
        "expected GatewayToDex when only gateway covers, got {:?}",
        quote.execution_path,
    );
}

/// USDC mint id used by the per-asset gate tests.
const PER_ASSET_USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
/// cbBTC placeholder mint matching the default `AppConfig` fixture, where
/// the real mainnet cbBTC mint is left as a `VERIFY_*` placeholder until the
/// operator has confirmed it.
const PER_ASSET_CBBTC_MINT: &str = "VERIFY_CBBTC_SOLANA_MINT_BEFORE_LIVE_USE";

fn rfq_with(input_mint: MintAddress, output_mint: MintAddress, amount_raw: u64) -> RfqRequest {
    RfqRequest {
        input_mint,
        output_mint,
        input_amount_raw: AmountRaw::new(amount_raw),
        taker_wallet: taker_wallet(),
        expiry_seconds: Some(30),
    }
}

#[tokio::test]
async fn rfq_rejects_input_below_asset_min_notional() {
    // Default per-asset USDC min is $1. $0.50 USDC must reject with the
    // per-asset reason — not the global cap.
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &sol(), 200_000_000);
    seed_working_custody(&persistence, &usdc(), 10_000_000);

    let orchestrator = persistent_harness_with_config(
        AppConfig::default(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory()]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq(500_000)) // $0.50 USDC
        .await
        .expect("request rfq");
    match response {
        RfqResponse::Rejected(rejection) => {
            assert_eq!(rejection.reason, RejectionReason::BelowAssetMinNotional);
        }
        RfqResponse::Accepted(quote) => {
            panic!("expected BelowAssetMinNotional rejection, got {quote:?}")
        }
    }
}

#[tokio::test]
async fn rfq_rejects_input_above_asset_max_notional() {
    // Default per-asset USDC max is $2. $3 USDC must reject with the
    // per-asset reason; the global $2 cap is also breached but the per-asset
    // gate runs first.
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &sol(), 200_000_000);
    seed_working_custody(&persistence, &usdc(), 10_000_000);

    let orchestrator = persistent_harness_with_config(
        AppConfig::default(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory()]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq(3_000_000)) // $3 USDC
        .await
        .expect("request rfq");
    match response {
        RfqResponse::Rejected(rejection) => {
            assert_eq!(rejection.reason, RejectionReason::AboveAssetMaxNotional);
        }
        RfqResponse::Accepted(quote) => {
            panic!("expected AboveAssetMaxNotional rejection, got {quote:?}")
        }
    }
}

#[tokio::test]
async fn rfq_rejects_output_above_asset_max_notional_when_input_passes() {
    // Bump USDC max to $10 so the input passes, but cbBTC max stays at $5.
    // Send $7 USDC -> cbBTC: input ($7) < USDC max ($10), output (~$7 cbBTC
    // worth) > cbBTC max ($5), so the gate must reject on the output side
    // with AboveAssetMaxNotional.
    let mut config = AppConfig::default();
    config.risk.max_quote_notional_usd = Decimal::from(20);
    config.risk.max_trade_notional_usd = Decimal::from(20);
    config.risk.max_daily_notional_usd = Decimal::from(100);
    config.risk.max_non_stable_asset_notional_usd = Decimal::from(20);
    for asset in &mut config.assets.supported {
        if asset.id.as_str() == "USDC" {
            asset.min_trade_notional_usd = Decimal::ONE;
            asset.max_trade_notional_usd = Decimal::from(10);
        }
    }

    // 1 USDC = 0.00002 cbBTC (i.e. cbBTC ~ $50_000) so $7 USDC -> ~0.00014
    // cbBTC, which is still ~$7 in USD value (well above the $5 cbBTC max).
    let price_provider = FakePriceProvider {
        prices: Arc::new(HashMap::from([
            (
                AssetPair::new(usdc(), cbbtc()),
                Decimal::new(2, 5), // 0.00002 cbBTC per USDC
            ),
            (AssetPair::new(cbbtc(), usdc()), Decimal::from(50_000)),
        ])),
    };

    let inventory = balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(20_000_000)),
        TokenAmount::new(sol(), AmountRaw::new(200_000_000)),
        TokenAmount::new(cbbtc(), AmountRaw::new(1_000_000)),
    ]);

    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &usdc(), 20_000_000);
    seed_working_custody(&persistence, &sol(), 200_000_000);
    seed_working_custody(&persistence, &cbbtc(), 1_000_000);

    let orchestrator = persistent_harness_with_config(
        config,
        price_provider,
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![inventory.clone(), inventory]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq_with(
            MintAddress::new(PER_ASSET_USDC_MINT),
            MintAddress::new(PER_ASSET_CBBTC_MINT),
            7_000_000, // $7 USDC
        ))
        .await
        .expect("request rfq");
    match response {
        RfqResponse::Rejected(rejection) => {
            assert_eq!(
                rejection.reason,
                RejectionReason::AboveAssetMaxNotional,
                "expected output-side AboveAssetMaxNotional, got {:?}",
                rejection.reason,
            );
        }
        RfqResponse::Accepted(quote) => {
            panic!("expected output-side AboveAssetMaxNotional, got {quote:?}")
        }
    }
}

#[tokio::test]
async fn rfq_accepts_when_both_sides_within_per_asset_range() {
    // Bootstrap with defaults. $1.50 USDC -> SOL: input ∈ [$1, $2] and the
    // SOL output value (~$1.50) ∈ [$1, $2]. Accept.
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &sol(), 200_000_000);
    seed_working_custody(&persistence, &usdc(), 10_000_000);

    let orchestrator = persistent_harness_with_config(
        AppConfig::default(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory(), quote_inventory()]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq(1_500_000)) // $1.50 USDC
        .await
        .expect("request rfq");
    match response {
        RfqResponse::Accepted(_) => {}
        RfqResponse::Rejected(rejection) => {
            panic!("expected Accepted within per-asset range, got {rejection:?}")
        }
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
    redeemed_legs: HashMap<TradeId, Vec<SettlementLeg>>,
    failure: Option<HtlcFailure>,
}

fn fake_leg_for_funder(funder: WalletRole) -> SettlementLeg {
    match funder {
        WalletRole::Maker => SettlementLeg::MakerOutput,
        WalletRole::Taker | WalletRole::Operator | WalletRole::Gateway => SettlementLeg::TakerInput,
    }
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
        let leg = fake_leg_for_funder(request.funder);
        let amount = request.amount.clone();
        state.initiated.push(request.clone());
        Ok(HtlcReceipt {
            trade_id: request.trade_id,
            leg,
            amount,
            status: SettlementStatus::Initiated,
            signature: Some(TxSignature::new(format!("init-{}", state.initiated.len()))),
        })
    }

    async fn redeem(&self, trade_id: TradeId, preimage: String) -> Result<HtlcReceipt, AppError> {
        let mut state = self.inner.lock().expect("htlc lock");
        let trade_initiations: Vec<HtlcInitiation> = state
            .initiated
            .iter()
            .filter(|init| init.trade_id == trade_id)
            .cloned()
            .collect();
        if trade_initiations.is_empty() {
            return Err(AppError::validation(format!(
                "fake htlc client: unknown HTLC trade {trade_id}"
            )));
        }

        // Mirror the production adapter: taker redeem (MakerOutput) runs first,
        // then maker redeem (TakerInput). Pick the first leg that has not yet
        // been redeemed for this trade.
        let already = state.redeemed_legs.entry(trade_id).or_default();
        let order = [SettlementLeg::MakerOutput, SettlementLeg::TakerInput];
        let leg = order
            .into_iter()
            .find(|candidate| {
                trade_initiations
                    .iter()
                    .any(|init| fake_leg_for_funder(init.funder) == *candidate)
                    && !already.contains(candidate)
            })
            .ok_or_else(|| {
                AppError::validation(format!(
                    "fake htlc client: all legs already redeemed for trade {trade_id}"
                ))
            })?;
        already.push(leg);
        let init = trade_initiations
            .iter()
            .find(|init| fake_leg_for_funder(init.funder) == leg)
            .expect("matching initiation exists");
        let amount = init.amount.clone();
        state.redeemed.push((trade_id, preimage));
        Ok(HtlcReceipt {
            trade_id,
            leg,
            amount,
            status: SettlementStatus::Redeemed,
            signature: Some(TxSignature::new(format!("redeem-{}", state.redeemed.len()))),
        })
    }

    async fn refund(&self, trade_id: TradeId) -> Result<HtlcReceipt, AppError> {
        let state = self.inner.lock().expect("htlc lock");
        let init = state
            .initiated
            .iter()
            .find(|init| init.trade_id == trade_id)
            .ok_or_else(|| {
                AppError::validation(format!("fake htlc client: unknown HTLC trade {trade_id}"))
            })?;
        let leg = fake_leg_for_funder(init.funder);
        let amount = init.amount.clone();
        Ok(HtlcReceipt {
            trade_id,
            leg,
            amount,
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
    fail_execute: Arc<Mutex<bool>>,
    output_override: Arc<Mutex<Option<TokenAmount>>>,
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

    fn executed_quotes(&self) -> Vec<SwapQuote> {
        self.executed.lock().expect("swap execute lock").clone()
    }

    fn with_execute_failure() -> Self {
        Self {
            fail_execute: Arc::new(Mutex::new(true)),
            ..Self::default()
        }
    }

    fn with_output_override(amount: TokenAmount) -> Self {
        Self {
            output_override: Arc::new(Mutex::new(Some(amount))),
            ..Self::default()
        }
    }
}

#[async_trait]
impl SwapExecutor for FakeSwapExecutor {
    async fn quote_swap(&self, request: SwapRequest) -> Result<SwapQuote, AppError> {
        self.quoted
            .lock()
            .expect("swap quote lock")
            .push(request.clone());
        let expected_output = self
            .output_override
            .lock()
            .expect("output override lock")
            .clone()
            .unwrap_or_else(|| TokenAmount::new(request.pair.output.clone(), AmountRaw::new(10)));
        Ok(SwapQuote {
            expected_output,
            estimated_fee: Some(TokenAmount::new(usdc(), AmountRaw::new(10_000))),
            expires_at: None,
            request,
        })
    }

    async fn execute_swap(&self, quote: SwapQuote) -> Result<SwapReceipt, AppError> {
        if *self.fail_execute.lock().expect("swap fail lock") {
            return Err(AppError::solana("fake swap execute failed"));
        }
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
    fail_refill: Arc<Mutex<bool>>,
    deposits: Arc<Mutex<Vec<AmountRaw>>>,
    fail_deposit: Arc<Mutex<bool>>,
}

impl FakeGatewayClient {
    fn refill_count(&self) -> usize {
        self.refills.lock().expect("gateway lock").len()
    }

    fn refill_requests(&self) -> Vec<GatewayRefillRequest> {
        self.refills.lock().expect("gateway lock").clone()
    }

    fn with_refill_failure() -> Self {
        Self {
            fail_refill: Arc::new(Mutex::new(true)),
            ..Self::default()
        }
    }

    fn deposit_count(&self) -> usize {
        self.deposits.lock().expect("gateway deposit lock").len()
    }

    fn deposit_amounts(&self) -> Vec<AmountRaw> {
        self.deposits.lock().expect("gateway deposit lock").clone()
    }

    fn with_deposit_failure() -> Self {
        Self {
            fail_deposit: Arc::new(Mutex::new(true)),
            ..Self::default()
        }
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
        if *self.fail_refill.lock().expect("gateway fail lock") {
            return Err(AppError::external_service(
                "circle_gateway",
                "fake gateway refill failed",
            ));
        }
        Ok(GatewayReceipt {
            amount: request.amount,
            provider_transfer_id: Some("gateway-transfer".to_owned()),
            signature: Some(TxSignature::new("gateway-sig")),
        })
    }

    async fn deposit(&self, amount_raw: AmountRaw) -> Result<GatewayReceipt, AppError> {
        self.deposits
            .lock()
            .expect("gateway deposit lock")
            .push(amount_raw);
        if *self.fail_deposit.lock().expect("gateway deposit fail lock") {
            return Err(AppError::external_service(
                "circle_gateway",
                "fake gateway deposit failed",
            ));
        }
        Ok(GatewayReceipt {
            amount: TokenAmount::new(usdc(), amount_raw),
            provider_transfer_id: Some("gateway-deposit".to_owned()),
            signature: Some(TxSignature::new("gateway-deposit-sig")),
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

fn run_id() -> firmament::types::RuntimeRunId {
    firmament::types::RuntimeRunId::generate()
}

fn settlement_event(event: SettlementEvent) -> RuntimeEvent {
    RuntimeEvent::Settlement(event)
}

fn htlc_receipt(
    trade_id: TradeId,
    leg: SettlementLeg,
    asset: AssetId,
    raw_amount: u64,
    status: SettlementStatus,
) -> HtlcReceipt {
    HtlcReceipt {
        trade_id,
        leg,
        amount: TokenAmount::new(asset, AmountRaw::new(raw_amount)),
        status,
        signature: Some(TxSignature::new(format!("{leg:?}-{trade_id}-{status:?}"))),
    }
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn inventory_path_emits_full_ledger_sequence() {
    // Drives the LedgerEventConsumer with a synthetic, ordered settlement
    // sequence and asserts ledger balances at every transition checkpoint.
    //
    // This is the load-bearing assertion for Task 5c: the consumer must
    // emit working_custody -> reserved -> pending_escrow -> htlc_escrow ->
    // trading on the maker output leg, and external -> receivable ->
    // working_custody on the taker input leg.

    let db = Db::open_in_memory().expect("open in-memory db");
    let consumer = LedgerEventConsumer::new(&db);
    let repository = SqliteLedgerRepository::new(&db);

    let trade_id = TradeId::generate();
    let run = run_id();

    // Seed working custody with the maker's output asset (SOL).
    let maker_output_amount: u64 = 500_000_000;
    let taker_input_amount: u64 = 1_000_000;
    let seed = firmament::ledger::LedgerTransactionBuilder::new("test_seed", trade_id.as_uuid())
        .description("seed maker SOL working custody")
        .idempotency_key(format!("seed:{trade_id}:working_sol"))
        .debit(
            LedgerAccountId::working(sol()),
            AmountRaw::new(maker_output_amount),
        )
        .credit(
            LedgerAccountId::external(sol(), "seed"),
            AmountRaw::new(maker_output_amount),
        )
        .build()
        .expect("balanced seed");
    repository.save_transaction(&seed).expect("save seed");

    // Sanity-check the seed.
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(sol()))
            .expect("working sol after seed"),
        i128::from(maker_output_amount)
    );

    // Step 1: Submitted{TakerInput} — informational, no movement.
    consumer
        .consume(&settlement_event(SettlementEvent::Submitted {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::TakerInput,
                usdc(),
                taker_input_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("submitted taker input");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::receivable(usdc(), trade_id.to_string()))
            .expect("receivable before confirmation"),
        0,
        "Submitted{{TakerInput}} must not open the receivable"
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(sol()))
            .expect("working sol after submitted taker input"),
        i128::from(maker_output_amount),
        "Submitted{{TakerInput}} must not touch maker working custody"
    );

    // Step 2: Confirmed{TakerInput} — opens receivable on input asset.
    consumer
        .consume(&settlement_event(SettlementEvent::Confirmed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::TakerInput,
                usdc(),
                taker_input_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("confirmed taker input");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::receivable(usdc(), trade_id.to_string()))
            .expect("receivable after confirmation"),
        i128::from(taker_input_amount)
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::external(usdc(), "htlc:taker_input"))
            .expect("external taker_input after confirmation"),
        -i128::from(taker_input_amount)
    );

    // Step 3: Submitted{MakerOutput} — emits working_custody -> reserved
    // and then reserved -> pending_escrow back-to-back.
    consumer
        .consume(&settlement_event(SettlementEvent::Submitted {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                sol(),
                maker_output_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("submitted maker output");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(sol()))
            .expect("working sol after maker submit"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::reserved(sol(), trade_id.to_string()))
            .expect("reserved after maker submit"),
        0,
        "reserved nets to zero after both transitions land"
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::pending_escrow(sol()))
            .expect("pending_escrow after maker submit"),
        i128::from(maker_output_amount)
    );

    // Step 4: Confirmed{MakerOutput} — pending_escrow -> htlc_escrow.
    consumer
        .consume(&settlement_event(SettlementEvent::Confirmed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                sol(),
                maker_output_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("confirmed maker output");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::pending_escrow(sol()))
            .expect("pending_escrow after maker confirm"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::htlc_escrow(sol()))
            .expect("htlc_escrow after maker confirm"),
        i128::from(maker_output_amount)
    );

    // Step 5: Redeemed{MakerOutput} — taker redeems maker leg into trading.
    consumer
        .consume(&settlement_event(SettlementEvent::Redeemed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                sol(),
                maker_output_amount,
                SettlementStatus::Redeemed,
            ),
        }))
        .expect("redeemed maker output");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::htlc_escrow(sol()))
            .expect("htlc_escrow after redeem maker"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::trading(sol()))
            .expect("trading after redeem maker"),
        i128::from(maker_output_amount)
    );

    // Step 6: Redeemed{TakerInput} — maker claims taker input into working.
    consumer
        .consume(&settlement_event(SettlementEvent::Redeemed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::TakerInput,
                usdc(),
                taker_input_amount,
                SettlementStatus::Redeemed,
            ),
        }))
        .expect("redeemed taker input");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::receivable(usdc(), trade_id.to_string()))
            .expect("receivable after maker redeem"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(usdc()))
            .expect("working usdc after maker redeem"),
        i128::from(taker_input_amount)
    );

    // Final invariants: ledger fully balanced per asset.
    let report = repository.integrity_report().expect("integrity report");
    assert!(report.healthy, "ledger should balance: {report:?}");
}

#[tokio::test]
async fn maker_output_refund_after_confirmation_unwinds_htlc_escrow() {
    // Drives the consumer through Submitted{MakerOutput} +
    // Confirmed{MakerOutput} so the trade lives in htlc_escrow, then a
    // Refunded{MakerOutput} must reverse htlc_escrow -> working_custody.
    let db = Db::open_in_memory().expect("open in-memory db");
    let consumer = LedgerEventConsumer::new(&db);
    let repository = SqliteLedgerRepository::new(&db);

    let trade_id = TradeId::generate();
    let run = run_id();
    let maker_output_amount: u64 = 250_000_000;

    let seed = firmament::ledger::LedgerTransactionBuilder::new("test_seed", trade_id.as_uuid())
        .description("seed maker SOL working custody")
        .idempotency_key(format!("seed:{trade_id}:working_sol"))
        .debit(
            LedgerAccountId::working(sol()),
            AmountRaw::new(maker_output_amount),
        )
        .credit(
            LedgerAccountId::external(sol(), "seed"),
            AmountRaw::new(maker_output_amount),
        )
        .build()
        .expect("balanced seed");
    repository.save_transaction(&seed).expect("save seed");

    consumer
        .consume(&settlement_event(SettlementEvent::Submitted {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                sol(),
                maker_output_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("submitted maker output");
    consumer
        .consume(&settlement_event(SettlementEvent::Confirmed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                sol(),
                maker_output_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("confirmed maker output");

    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::htlc_escrow(sol()))
            .expect("htlc_escrow before refund"),
        i128::from(maker_output_amount)
    );

    consumer
        .consume(&settlement_event(SettlementEvent::Refunded {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                sol(),
                maker_output_amount,
                SettlementStatus::Refunded,
            ),
        }))
        .expect("refunded maker output");

    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::htlc_escrow(sol()))
            .expect("htlc_escrow after refund"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(sol()))
            .expect("working sol after refund"),
        i128::from(maker_output_amount)
    );
}

#[tokio::test]
async fn taker_input_refund_closes_receivable_back_to_external() {
    // Drives Confirmed{TakerInput} (opens receivable), then
    // Refunded{TakerInput} (closes the receivable back into external).
    let db = Db::open_in_memory().expect("open in-memory db");
    let consumer = LedgerEventConsumer::new(&db);
    let repository = SqliteLedgerRepository::new(&db);

    let trade_id = TradeId::generate();
    let run = run_id();
    let taker_input_amount: u64 = 750_000;

    consumer
        .consume(&settlement_event(SettlementEvent::Confirmed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::TakerInput,
                usdc(),
                taker_input_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("confirmed taker input");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::receivable(usdc(), trade_id.to_string()))
            .expect("receivable after confirm"),
        i128::from(taker_input_amount)
    );

    consumer
        .consume(&settlement_event(SettlementEvent::Refunded {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::TakerInput,
                usdc(),
                taker_input_amount,
                SettlementStatus::Refunded,
            ),
        }))
        .expect("refunded taker input");

    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::receivable(usdc(), trade_id.to_string()))
            .expect("receivable after refund"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::external(usdc(), "htlc:taker_input"))
            .expect("external taker_input after refund"),
        0
    );

    let report = repository.integrity_report().expect("integrity report");
    assert!(report.healthy, "refund should leave ledger balanced");
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn settlement_gateway_usdc_path_emits_full_ledger_sequence() {
    // GatewayToDex path with USDC output. Drives the LedgerEventConsumer with
    // the full Gateway-backed sequence and asserts each transition.
    //
    // Per design § 2 Gateway-backed USDC table:
    //   1. Confirmed{TakerInput}        external -> receivable (taker input asset)
    //   2. Gateway::BurnIntentSubmitted  gateway -> gateway_reserved -> trading
    //   3. Gateway::MintConfirmed       trading -> working_custody (USDC)
    //   4. Submitted{MakerOutput}       working_custody -> reserved -> pending_escrow
    //   5. Confirmed{MakerOutput}       pending_escrow -> htlc_escrow
    //   6. Redeemed{MakerOutput}        htlc_escrow -> trading
    //   7. Redeemed{TakerInput}         receivable -> working_custody
    let db = Db::open_in_memory().expect("open in-memory db");
    let consumer = LedgerEventConsumer::new(&db);
    let repository = SqliteLedgerRepository::new(&db);

    let trade_id = TradeId::generate();
    let run = run_id();

    // For Gateway-backed paths the maker has nothing in working_custody for
    // the output asset at quote time — that's why the path was selected.
    // Seed gateway:USDC to fund the burn.
    let gateway_amount: u64 = 1_000_000_000;
    let burn_amount: u64 = 500_000_000;
    let taker_input_amount: u64 = 500_000_000; // input also USDC for this scenario
    let seed_gateway = firmament::ledger::LedgerTransactionBuilder::new(
        "test_seed_gateway_usdc",
        trade_id.as_uuid(),
    )
    .description("seed maker gateway USDC")
    .idempotency_key(format!("seed:{trade_id}:gateway_usdc"))
    .debit(
        LedgerAccountId::gateway(usdc()),
        AmountRaw::new(gateway_amount),
    )
    .credit(
        LedgerAccountId::external(usdc(), "seed_gateway"),
        AmountRaw::new(gateway_amount),
    )
    .build()
    .expect("balanced gateway seed");
    repository
        .save_transaction(&seed_gateway)
        .expect("save gateway seed");

    // Step 1: Confirmed{TakerInput} on input asset (use a different asset than
    // USDC so receivable/working_custody assertions stay distinct).
    consumer
        .consume(&settlement_event(SettlementEvent::Confirmed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::TakerInput,
                sol(),
                taker_input_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("confirmed taker input");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::receivable(sol(), trade_id.to_string()))
            .expect("receivable after taker confirm"),
        i128::from(taker_input_amount)
    );

    // Step 2: BurnIntentSubmitted (compound: gateway -> gateway_reserved
    // -> trading). After both transitions land, gateway_reserved nets to zero.
    consumer
        .consume(&RuntimeEvent::Gateway(GatewayEvent::BurnIntentSubmitted {
            metadata: EventMetadata::new(run),
            trade_id,
            amount: TokenAmount::new(usdc(), AmountRaw::new(burn_amount)),
            signature: None,
        }))
        .expect("burn intent submitted");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::gateway(usdc()))
            .expect("gateway after burn"),
        i128::from(gateway_amount - burn_amount)
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::gateway_reserved(
                usdc(),
                trade_id.to_string()
            ))
            .expect("gateway_reserved after burn"),
        0,
        "gateway_reserved nets to zero after burn"
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::trading(usdc()))
            .expect("trading after burn"),
        i128::from(burn_amount)
    );

    // Step 3: MintConfirmed → trading -> working_custody.
    consumer
        .consume(&RuntimeEvent::Gateway(GatewayEvent::MintConfirmed {
            metadata: EventMetadata::new(run),
            trade_id,
            amount: TokenAmount::new(usdc(), AmountRaw::new(burn_amount)),
            signature: None,
        }))
        .expect("mint confirmed");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::trading(usdc()))
            .expect("trading after mint"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(usdc()))
            .expect("working USDC after mint"),
        i128::from(burn_amount)
    );

    // Step 4: Submitted{MakerOutput} on USDC.
    consumer
        .consume(&settlement_event(SettlementEvent::Submitted {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                usdc(),
                burn_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("submitted maker output");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(usdc()))
            .expect("working USDC after maker submit"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::pending_escrow(usdc()))
            .expect("pending_escrow after maker submit"),
        i128::from(burn_amount)
    );

    // Step 5: Confirmed{MakerOutput}.
    consumer
        .consume(&settlement_event(SettlementEvent::Confirmed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                usdc(),
                burn_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("confirmed maker output");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::htlc_escrow(usdc()))
            .expect("htlc_escrow after maker confirm"),
        i128::from(burn_amount)
    );

    // Step 6: Redeemed{MakerOutput}.
    consumer
        .consume(&settlement_event(SettlementEvent::Redeemed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                usdc(),
                burn_amount,
                SettlementStatus::Redeemed,
            ),
        }))
        .expect("redeemed maker output");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::htlc_escrow(usdc()))
            .expect("htlc_escrow after maker redeem"),
        0
    );

    // Step 7: Redeemed{TakerInput}.
    consumer
        .consume(&settlement_event(SettlementEvent::Redeemed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::TakerInput,
                sol(),
                taker_input_amount,
                SettlementStatus::Redeemed,
            ),
        }))
        .expect("redeemed taker input");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::receivable(sol(), trade_id.to_string()))
            .expect("receivable after maker redeem"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(sol()))
            .expect("working SOL after maker redeem"),
        i128::from(taker_input_amount)
    );

    let report = repository.integrity_report().expect("integrity report");
    assert!(report.healthy, "ledger should balance: {report:?}");
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn settlement_gateway_sol_path_emits_jupiter_spend_sequence() {
    // GatewayToDex path with SOL output. Drives:
    //   1. Confirmed{TakerInput}        external -> receivable (input asset)
    //   2. BurnIntentSubmitted          gateway -> gateway_reserved -> trading (USDC)
    //   3. MintConfirmed                trading -> working_custody (USDC)
    //   4. TradeSwapSubmitted           working_custody:USDC -> pending_dex_spend:USDC
    //   5. TradeSwapConfirmed           pending_dex_spend:USDC -> trading:USDC
    //                                    AND trading:SOL -> working_custody:SOL
    //   6. Submitted{MakerOutput}       working_custody:SOL -> reserved:SOL -> pending_escrow:SOL
    //   7. Confirmed{MakerOutput}       pending_escrow -> htlc_escrow
    //   8. Redeemed{MakerOutput}        htlc_escrow -> trading
    //   9. Redeemed{TakerInput}         receivable -> working_custody
    let db = Db::open_in_memory().expect("open in-memory db");
    let consumer = LedgerEventConsumer::new(&db);
    let repository = SqliteLedgerRepository::new(&db);

    let trade_id = TradeId::generate();
    let run = run_id();

    let gateway_amount: u64 = 1_000_000_000;
    let usdc_input_amount: u64 = 500_000_000; // taker delivered USDC
    let sol_output_amount: u64 = 2_500_000_000; // 2.5 SOL ($500 at $200/SOL)
    let usdc_swap_amount: u64 = 500_000_000;

    let seed_gateway = firmament::ledger::LedgerTransactionBuilder::new(
        "test_seed_gateway_sol",
        trade_id.as_uuid(),
    )
    .description("seed maker gateway USDC for SOL path")
    .idempotency_key(format!("seed:{trade_id}:gateway_usdc_sol"))
    .debit(
        LedgerAccountId::gateway(usdc()),
        AmountRaw::new(gateway_amount),
    )
    .credit(
        LedgerAccountId::external(usdc(), "seed_gateway"),
        AmountRaw::new(gateway_amount),
    )
    .build()
    .expect("balanced gateway seed");
    repository
        .save_transaction(&seed_gateway)
        .expect("save gateway seed");

    // Step 1: Confirmed{TakerInput}.
    consumer
        .consume(&settlement_event(SettlementEvent::Confirmed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::TakerInput,
                usdc(),
                usdc_input_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("confirmed taker input");

    // Step 2: BurnIntentSubmitted.
    consumer
        .consume(&RuntimeEvent::Gateway(GatewayEvent::BurnIntentSubmitted {
            metadata: EventMetadata::new(run),
            trade_id,
            amount: TokenAmount::new(usdc(), AmountRaw::new(usdc_swap_amount)),
            signature: None,
        }))
        .expect("burn intent submitted");

    // Step 3: MintConfirmed.
    consumer
        .consume(&RuntimeEvent::Gateway(GatewayEvent::MintConfirmed {
            metadata: EventMetadata::new(run),
            trade_id,
            amount: TokenAmount::new(usdc(), AmountRaw::new(usdc_swap_amount)),
            signature: None,
        }))
        .expect("mint confirmed");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(usdc()))
            .expect("working USDC after mint"),
        i128::from(usdc_swap_amount)
    );

    // Step 4: TradeSwapSubmitted.
    consumer
        .consume(&RuntimeEvent::Swap(SwapEvent::TradeSwapSubmitted {
            metadata: EventMetadata::new(run),
            trade_id,
            input_amount: TokenAmount::new(usdc(), AmountRaw::new(usdc_swap_amount)),
            signature: None,
        }))
        .expect("trade swap submitted");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(usdc()))
            .expect("working USDC after swap submit"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::pending_dex_spend(
                usdc(),
                trade_id.to_string()
            ))
            .expect("pending_dex_spend after swap submit"),
        i128::from(usdc_swap_amount)
    );

    // Step 5: TradeSwapConfirmed.
    consumer
        .consume(&RuntimeEvent::Swap(SwapEvent::TradeSwapConfirmed {
            metadata: EventMetadata::new(run),
            trade_id,
            input_amount: TokenAmount::new(usdc(), AmountRaw::new(usdc_swap_amount)),
            output_amount: TokenAmount::new(sol(), AmountRaw::new(sol_output_amount)),
            signature: None,
        }))
        .expect("trade swap confirmed");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::pending_dex_spend(
                usdc(),
                trade_id.to_string()
            ))
            .expect("pending_dex_spend after swap confirm"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(sol()))
            .expect("working SOL after swap confirm"),
        i128::from(sol_output_amount)
    );

    // Step 6: Submitted{MakerOutput} on SOL.
    consumer
        .consume(&settlement_event(SettlementEvent::Submitted {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                sol(),
                sol_output_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("submitted maker output");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(sol()))
            .expect("working SOL after maker submit"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::pending_escrow(sol()))
            .expect("pending_escrow after maker submit"),
        i128::from(sol_output_amount)
    );

    // Step 7: Confirmed{MakerOutput}.
    consumer
        .consume(&settlement_event(SettlementEvent::Confirmed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                sol(),
                sol_output_amount,
                SettlementStatus::Initiated,
            ),
        }))
        .expect("confirmed maker output");

    // Step 8: Redeemed{MakerOutput}.
    consumer
        .consume(&settlement_event(SettlementEvent::Redeemed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::MakerOutput,
                sol(),
                sol_output_amount,
                SettlementStatus::Redeemed,
            ),
        }))
        .expect("redeemed maker output");

    // Step 9: Redeemed{TakerInput}.
    consumer
        .consume(&settlement_event(SettlementEvent::Redeemed {
            metadata: EventMetadata::new(run),
            receipt: htlc_receipt(
                trade_id,
                SettlementLeg::TakerInput,
                usdc(),
                usdc_input_amount,
                SettlementStatus::Redeemed,
            ),
        }))
        .expect("redeemed taker input");

    let report = repository.integrity_report().expect("integrity report");
    assert!(report.healthy, "ledger should balance: {report:?}");
}

#[tokio::test]
async fn settlement_gateway_failure_releases_reservation() {
    // Drives the consumer through a successful BurnIntentSubmitted (which
    // moves gateway -> trading via the back-to-back reserve+burn) and then a
    // BurnFailed: the latter must reverse trading -> gateway so the maker's
    // gateway balance ends up at the original seed value.
    let db = Db::open_in_memory().expect("open in-memory db");
    let consumer = LedgerEventConsumer::new(&db);
    let repository = SqliteLedgerRepository::new(&db);

    let trade_id = TradeId::generate();
    let run = run_id();
    let gateway_amount: u64 = 1_000_000_000;
    let burn_amount: u64 = 250_000_000;

    let seed = firmament::ledger::LedgerTransactionBuilder::new(
        "test_seed_gateway_burn",
        trade_id.as_uuid(),
    )
    .description("seed gateway USDC for burn-failure test")
    .idempotency_key(format!("seed:{trade_id}:gateway_burn_failure"))
    .debit(
        LedgerAccountId::gateway(usdc()),
        AmountRaw::new(gateway_amount),
    )
    .credit(
        LedgerAccountId::external(usdc(), "seed_gateway"),
        AmountRaw::new(gateway_amount),
    )
    .build()
    .expect("balanced gateway seed");
    repository.save_transaction(&seed).expect("save seed");

    consumer
        .consume(&RuntimeEvent::Gateway(GatewayEvent::BurnIntentSubmitted {
            metadata: EventMetadata::new(run),
            trade_id,
            amount: TokenAmount::new(usdc(), AmountRaw::new(burn_amount)),
            signature: None,
        }))
        .expect("burn intent submitted");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::trading(usdc()))
            .expect("trading after burn"),
        i128::from(burn_amount)
    );

    consumer
        .consume(&RuntimeEvent::Gateway(GatewayEvent::BurnFailed {
            metadata: EventMetadata::new(run),
            trade_id,
            amount: TokenAmount::new(usdc(), AmountRaw::new(burn_amount)),
            reason: "gateway adapter timeout".to_owned(),
        }))
        .expect("burn failed");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::trading(usdc()))
            .expect("trading after burn failure"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::gateway(usdc()))
            .expect("gateway after burn failure"),
        i128::from(gateway_amount),
        "gateway balance should be fully released back"
    );
}

#[tokio::test]
async fn settlement_jupiter_failure_unwinds_pending_dex_spend() {
    // Drives TradeSwapSubmitted (working_custody -> pending_dex_spend) and
    // then TradeSwapFailed: the latter must reverse pending_dex_spend ->
    // working_custody.
    let db = Db::open_in_memory().expect("open in-memory db");
    let consumer = LedgerEventConsumer::new(&db);
    let repository = SqliteLedgerRepository::new(&db);

    let trade_id = TradeId::generate();
    let run = run_id();
    let usdc_amount: u64 = 300_000_000;

    let seed = firmament::ledger::LedgerTransactionBuilder::new(
        "test_seed_swap_failure",
        trade_id.as_uuid(),
    )
    .description("seed working USDC for swap-failure test")
    .idempotency_key(format!("seed:{trade_id}:swap_failure"))
    .debit(
        LedgerAccountId::working(usdc()),
        AmountRaw::new(usdc_amount),
    )
    .credit(
        LedgerAccountId::external(usdc(), "seed"),
        AmountRaw::new(usdc_amount),
    )
    .build()
    .expect("balanced seed");
    repository.save_transaction(&seed).expect("save seed");

    consumer
        .consume(&RuntimeEvent::Swap(SwapEvent::TradeSwapSubmitted {
            metadata: EventMetadata::new(run),
            trade_id,
            input_amount: TokenAmount::new(usdc(), AmountRaw::new(usdc_amount)),
            signature: None,
        }))
        .expect("trade swap submitted");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::pending_dex_spend(
                usdc(),
                trade_id.to_string()
            ))
            .expect("pending_dex_spend after submit"),
        i128::from(usdc_amount)
    );

    consumer
        .consume(&RuntimeEvent::Swap(SwapEvent::TradeSwapFailed {
            metadata: EventMetadata::new(run),
            trade_id,
            input_amount: TokenAmount::new(usdc(), AmountRaw::new(usdc_amount)),
            reason: "jupiter route stale".to_owned(),
        }))
        .expect("trade swap failed");
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::pending_dex_spend(
                usdc(),
                trade_id.to_string()
            ))
            .expect("pending_dex_spend after failure"),
        0
    );
    assert_eq!(
        repository
            .account_balance(&LedgerAccountId::working(usdc()))
            .expect("working USDC after failure"),
        i128::from(usdc_amount),
        "working_custody should be fully restored"
    );
}

#[tokio::test]
async fn settlement_repricing_failure_skips_reservation() {
    // Drive the orchestrator with a quote that expires before acceptance.
    // The accept path must:
    //   1. Not move working_custody -> reserved (no Submitted{MakerOutput} fires).
    //   2. Emit a Settlement::Failed with reason "repricing_failed".
    //   3. Record a RuntimeTrade in Failed state for operator visibility.
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &sol(), 200_000_000);
    seed_working_custody(&persistence, &usdc(), 10_000_000);

    let orchestrator = persistent_harness_with_persistence(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![quote_inventory(), quote_inventory()]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    let mut request = rfq(100_000);
    request.expiry_seconds = Some(1);
    let response = orchestrator
        .request_rfq(request)
        .await
        .expect("request quote");
    let quote = match response {
        RfqResponse::Accepted(quote) => quote,
        RfqResponse::Rejected(rejection) => panic!("expected accepted: {rejection:?}"),
    };
    let quote_id = quote.quote_id;

    // Wait past the quote expiry so accept_quote enters the Expired branch.
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

    let error = orchestrator
        .accept_quote(quote_id)
        .await
        .expect_err("expired quote acceptance should fail");
    assert!(error.to_string().contains("expired"));

    // The expiry handler emits a Settlement::Failed with reason
    // "repricing_failed" — find it in the runtime event stream.
    let events = orchestrator.runtime().recent_events(None).await;
    let saw_repricing_failed = events.iter().any(|event| {
        matches!(
            event,
            RuntimeEvent::Settlement(SettlementEvent::Failed { reason, .. })
                if reason == "repricing_failed"
        )
    });
    assert!(
        saw_repricing_failed,
        "expected Settlement::Failed reason=repricing_failed in events: {events:#?}"
    );

    let trade_id = events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::Settlement(SettlementEvent::Failed {
                trade_id, reason, ..
            }) if reason == "repricing_failed" => Some(*trade_id),
            _ => None,
        })
        .expect("repricing_failed event carries trade_id");

    let runtime_trade = orchestrator
        .trade(trade_id)
        .await
        .expect("expired-quote trade recorded for visibility");
    assert_eq!(runtime_trade.settlement_status, SettlementStatus::Failed);

    // No Submitted settlement events should have fired (those carry the
    // working_custody -> reserved compound move on the maker leg).
    assert!(
        events.iter().all(|event| {
            !matches!(
                event,
                RuntimeEvent::Settlement(SettlementEvent::Submitted { .. })
            )
        }),
        "no Submitted settlement events should fire on repricing failure"
    );
}

// ---------- Phase 4 Gap 1: Gateway-backed adapter wiring tests ----------

/// Build a config and price book that lets a USDC-output RFQ accept on the
/// Gateway path: SOL input → USDC output, `working_custody:USDC` = 0, gateway
/// USDC seeded.
fn sol_to_usdc_rfq(amount_raw: u64) -> RfqRequest {
    RfqRequest {
        input_mint: sol_mint(),
        output_mint: usdc_mint(),
        input_amount_raw: AmountRaw::new(amount_raw),
        taker_wallet: taker_wallet(),
        expiry_seconds: Some(30),
    }
}

fn relaxed_gateway_path_config() -> AppConfig {
    let mut config = AppConfig::default();
    config.risk.max_quote_notional_usd = Decimal::from(1_000);
    config.risk.max_trade_notional_usd = Decimal::from(1_000);
    config.risk.max_daily_notional_usd = Decimal::from(10_000);
    config.risk.max_non_stable_asset_notional_usd = Decimal::from(1_000);
    relax_asset_notional_limits(&mut config);
    config
}

fn gateway_path_price_provider() -> FakePriceProvider {
    // SOL = $200 reference: 1 SOL = 200 USDC, 1 USDC = 0.005 SOL.
    FakePriceProvider {
        prices: Arc::new(HashMap::from([
            (AssetPair::new(usdc(), sol()), Decimal::new(5, 3)),
            (AssetPair::new(sol(), usdc()), Decimal::from(200)),
        ])),
    }
}

#[tokio::test]
async fn settlement_gateway_usdc_path_invokes_real_gateway_adapter() {
    // Gateway-backed USDC-output path: SOL input → USDC output. Working custody
    // for USDC is zero so the path resolves to GatewayToDex. The orchestrator
    // must call FakeGatewayClient::request_refill but skip the swap executor
    // entirely (output is USDC).
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &sol(), 200_000_000); // input present
    seed_gateway_balance(&persistence, &usdc(), 2_000_000_000);

    let zero_usdc_inventory = balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(0)),
        TokenAmount::new(sol(), AmountRaw::new(200_000_000)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ]);

    let gateway = FakeGatewayClient::default();
    let swap = FakeSwapExecutor::default();
    let orchestrator = persistent_harness_with_config(
        relaxed_gateway_path_config(),
        gateway_path_price_provider(),
        FakeHtlcClient::default(),
        swap.clone(),
        gateway.clone(),
        FakeBalanceReader::new(vec![zero_usdc_inventory.clone(), zero_usdc_inventory]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    // 0.005 SOL input → 1.0 USDC output @ $200/SOL.
    let response = orchestrator
        .request_rfq(sol_to_usdc_rfq(5_000_000))
        .await
        .expect("request rfq");
    let quote = match response {
        RfqResponse::Accepted(quote) => quote,
        RfqResponse::Rejected(rejection) => panic!("expected accepted: {rejection:?}"),
    };
    assert_eq!(quote.execution_path, ExecutionPath::GatewayToDex);
    let trade = orchestrator
        .accept_quote(quote.quote_id)
        .await
        .expect("settlement succeeds");

    // Adapter contract: gateway.request_refill invoked at least once for USDC
    // (the in-trade burn). Post-settlement rebalance automation may also
    // invoke a refill on top of the trade-bound one.
    assert!(
        gateway.refill_count() >= 1,
        "expected at least one trade-bound gateway refill, got {}",
        gateway.refill_count()
    );
    let trade_refill = gateway
        .refill_requests()
        .into_iter()
        .next()
        .expect("trade-bound refill must be the first call");
    assert_eq!(trade_refill.amount.asset, usdc());
    assert_eq!(trade_refill.destination, WalletRole::Maker);

    // No trade-correlated Jupiter swap event for the USDC-output path
    // (post-settlement automation rebalance swaps may fire but those are
    // SwapEvent::Quoted/Executed, not the TradeSwap variants).
    let events = orchestrator.runtime().recent_events(None).await;
    assert!(
        !events.iter().any(|event| {
            matches!(
                event,
                RuntimeEvent::Swap(
                    SwapEvent::TradeSwapSubmitted { .. } | SwapEvent::TradeSwapConfirmed { .. }
                )
            )
        }),
        "USDC-output gateway path must not emit trade-correlated swap events"
    );

    // Trade signature kinds must include GatewayBurn + GatewayMint.
    let kinds: Vec<TradeSignatureKind> = trade.tx_signature_kinds.iter().map(|s| s.kind).collect();
    assert!(
        kinds.contains(&TradeSignatureKind::GatewayBurn),
        "expected GatewayBurn in {kinds:?}",
    );
    assert!(
        kinds.contains(&TradeSignatureKind::GatewayMint),
        "expected GatewayMint in {kinds:?}",
    );
    assert!(
        !kinds.contains(&TradeSignatureKind::JupiterSwap),
        "JupiterSwap should not appear for USDC-output gateway path: {kinds:?}",
    );
    let summary = orchestrator
        .ledger_summary()
        .expect("balanced ledger summary");
    assert!(summary.balanced, "ledger should be balanced after trade");
}

#[tokio::test]
async fn settlement_gateway_sol_path_invokes_real_gateway_and_jupiter() {
    // Gateway-backed SOL-output path: USDC input → SOL output. Working custody
    // for SOL is zero so the path resolves to GatewayToDex. The orchestrator
    // must call FakeGatewayClient::request_refill AND FakeSwapExecutor with a
    // USDC -> SOL pair.
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &usdc(), 1_000_000_000); // input present
    seed_gateway_balance(&persistence, &usdc(), 2_000_000_000);

    let zero_sol_inventory = balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(1_000_000_000)),
        TokenAmount::new(sol(), AmountRaw::new(0)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ]);

    let gateway = FakeGatewayClient::default();
    // Override swap output so the confirmed amount lands non-zero on SOL —
    // the orchestrator drives a swap for `input_amount` (USDC), and the fake
    // would otherwise return SwapQuote.expected_output of 10 lamports.
    let swap =
        FakeSwapExecutor::with_output_override(TokenAmount::new(sol(), AmountRaw::new(1_000_000)));
    let orchestrator = persistent_harness_with_config(
        relaxed_gateway_path_config(),
        gateway_path_price_provider(),
        FakeHtlcClient::default(),
        swap.clone(),
        gateway.clone(),
        FakeBalanceReader::new(vec![zero_sol_inventory.clone(), zero_sol_inventory]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    // 200 USDC input → 1.0 SOL output.
    let response = orchestrator
        .request_rfq(rfq(200_000_000))
        .await
        .expect("request rfq");
    let quote = match response {
        RfqResponse::Accepted(quote) => quote,
        RfqResponse::Rejected(rejection) => panic!("expected accepted: {rejection:?}"),
    };
    assert_eq!(quote.execution_path, ExecutionPath::GatewayToDex);
    let trade = orchestrator
        .accept_quote(quote.quote_id)
        .await
        .expect("settlement succeeds");

    // Gateway invoked at least once for the trade-bound burn (post-settlement
    // automation may add another refill, which is fine).
    assert!(gateway.refill_count() >= 1);
    // Jupiter swap invoked at least once for the trade-bound USDC -> SOL
    // swap (post-settlement automation may add additional rebalance swaps).
    assert!(
        swap.quoted_count() >= 1,
        "jupiter quote must be requested at least once"
    );
    assert!(
        swap.executed_count() >= 1,
        "jupiter execute must be invoked at least once"
    );
    let trade_swap = swap
        .executed_quotes()
        .into_iter()
        .find(|q| q.request.pair.input == usdc() && q.request.pair.output == sol())
        .expect("trade-bound USDC -> SOL swap must be executed");
    assert_eq!(trade_swap.request.source_wallet, WalletRole::Maker);
    assert_eq!(trade_swap.request.destination_wallet, WalletRole::Maker);

    // Trade signature kinds must include GatewayBurn + GatewayMint + JupiterSwap.
    let kinds: Vec<TradeSignatureKind> = trade.tx_signature_kinds.iter().map(|s| s.kind).collect();
    assert!(
        kinds.contains(&TradeSignatureKind::GatewayBurn),
        "expected GatewayBurn in {kinds:?}",
    );
    assert!(
        kinds.contains(&TradeSignatureKind::GatewayMint),
        "expected GatewayMint in {kinds:?}",
    );
    assert!(
        kinds.contains(&TradeSignatureKind::JupiterSwap),
        "expected JupiterSwap in {kinds:?}",
    );
}

#[tokio::test]
async fn settlement_gateway_failure_releases_reservation_and_does_not_attempt_jupiter() {
    // FakeGatewayClient configured to fail on request_refill. The orchestrator
    // must:
    //   1. Emit Gateway::Failed (the adapter is atomic — neither burn nor
    //      mint landed, so no compensating BurnFailed move is needed).
    //   2. NOT invoke the swap executor (failure short-circuits).
    //   3. NOT submit the maker HTLC leg.
    //   4. Bubble up an external_service error from accept_quote.
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &usdc(), 1_000_000_000);
    seed_gateway_balance(&persistence, &usdc(), 2_000_000_000);

    let zero_sol_inventory = balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(1_000_000_000)),
        TokenAmount::new(sol(), AmountRaw::new(0)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ]);

    let gateway = FakeGatewayClient::with_refill_failure();
    let swap = FakeSwapExecutor::default();
    let htlc = FakeHtlcClient::default();
    let orchestrator = persistent_harness_with_config(
        relaxed_gateway_path_config(),
        gateway_path_price_provider(),
        htlc.clone(),
        swap.clone(),
        gateway.clone(),
        FakeBalanceReader::new(vec![zero_sol_inventory.clone(), zero_sol_inventory]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq(200_000_000))
        .await
        .expect("request rfq");
    let quote = match response {
        RfqResponse::Accepted(quote) => quote,
        RfqResponse::Rejected(rejection) => panic!("expected accepted: {rejection:?}"),
    };
    assert_eq!(quote.execution_path, ExecutionPath::GatewayToDex);
    let error = orchestrator
        .accept_quote(quote.quote_id)
        .await
        .expect_err("gateway failure must abort settlement");
    assert!(
        error.to_string().contains("gateway"),
        "error should mention gateway: {error}"
    );

    // Gateway was attempted; jupiter never was; only taker HTLC initiated (no maker).
    assert_eq!(gateway.refill_count(), 1);
    assert_eq!(swap.quoted_count(), 0);
    assert_eq!(swap.executed_count(), 0);
    assert_eq!(
        htlc.initiated_count(),
        1,
        "taker HTLC initiates before the gateway slice; maker leg must NOT initiate"
    );

    // Gateway::Failed event fired (atomic adapter failure: no
    // BurnIntentSubmitted ever published, so no BurnFailed compensation
    // either; surface the failure for observers via Gateway::Failed).
    let events = orchestrator.runtime().recent_events(None).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Gateway(GatewayEvent::Failed { .. }))),
        "expected Gateway::Failed event"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Gateway(GatewayEvent::BurnIntentSubmitted { .. })
        )),
        "BurnIntentSubmitted must not fire when request_refill aborts before any on-chain effect"
    );

    // Ledger reverses the reservation: gateway:USDC restored to its full
    // seeded value (2 B raw) after burn failure.
    let gateway_balance = persistence
        .account_balance(&LedgerAccountId::gateway(usdc()))
        .expect("gateway balance");
    assert_eq!(
        gateway_balance, 2_000_000_000,
        "gateway:USDC must be fully released back after burn failure"
    );
}

#[tokio::test]
async fn settlement_jupiter_failure_after_gateway_unwinds_pending_dex_spend() {
    // Gateway succeeds, jupiter fails on execute_swap. The orchestrator must:
    //   1. Have produced BurnIntentSubmitted + MintConfirmed events
    //      (working_custody:USDC populated for the trade).
    //   2. Emit TradeSwapFailed.
    //   3. NOT submit the maker HTLC leg.
    //   4. Bubble up the swap error from accept_quote.
    //   5. Leave the ledger with pending_dex_spend reverted to working_custody.
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &usdc(), 1_000_000_000);
    seed_gateway_balance(&persistence, &usdc(), 2_000_000_000);

    let zero_sol_inventory = balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(1_000_000_000)),
        TokenAmount::new(sol(), AmountRaw::new(0)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ]);

    let gateway = FakeGatewayClient::default();
    let swap = FakeSwapExecutor::with_execute_failure();
    let htlc = FakeHtlcClient::default();
    let orchestrator = persistent_harness_with_config(
        relaxed_gateway_path_config(),
        gateway_path_price_provider(),
        htlc.clone(),
        swap.clone(),
        gateway.clone(),
        FakeBalanceReader::new(vec![zero_sol_inventory.clone(), zero_sol_inventory]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq(200_000_000))
        .await
        .expect("request rfq");
    let quote = match response {
        RfqResponse::Accepted(quote) => quote,
        RfqResponse::Rejected(rejection) => panic!("expected accepted: {rejection:?}"),
    };
    assert_eq!(quote.execution_path, ExecutionPath::GatewayToDex);
    let error = orchestrator
        .accept_quote(quote.quote_id)
        .await
        .expect_err("jupiter failure must abort settlement");
    assert!(
        error.to_string().to_lowercase().contains("jupiter")
            || error.to_string().to_lowercase().contains("swap"),
        "error should mention swap/jupiter: {error}"
    );

    // Gateway succeeded (refill called once), Jupiter quoted + execute attempted.
    assert_eq!(gateway.refill_count(), 1);
    assert_eq!(swap.quoted_count(), 1);
    assert_eq!(
        swap.executed_count(),
        0,
        "execute_swap returned Err so the recorded list stays empty"
    );
    assert_eq!(
        htlc.initiated_count(),
        1,
        "only the taker HTLC initiates; maker leg must NOT initiate after swap failure"
    );

    // Both Gateway success events landed; TradeSwapFailed event landed.
    let events = orchestrator.runtime().recent_events(None).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Gateway(GatewayEvent::BurnIntentSubmitted { .. })
        )),
        "expected Gateway::BurnIntentSubmitted",
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Gateway(GatewayEvent::MintConfirmed { .. })
        )),
        "expected Gateway::MintConfirmed",
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Swap(SwapEvent::TradeSwapFailed { .. }))),
        "expected Swap::TradeSwapFailed",
    );

    // Ledger should reverse pending_dex_spend → working_custody for USDC.
    // The trade-qualified pending_dex_spend account nets to zero after the
    // unwind. (Aggregate by account type so the test does not need to know
    // the trade_id qualifier string.)
    let pending_dex_spend = persistence
        .aggregate_balance_by_type(
            firmament::ledger::LedgerAccountType::PendingDexSpend,
            &usdc(),
        )
        .expect("pending_dex_spend aggregate");
    assert_eq!(
        pending_dex_spend, 0,
        "pending_dex_spend should be unwound after swap failure"
    );
}

// ---------------------------------------------------------------------------
// Always-on automation worker tests (Phase 4 Gap 3).
// ---------------------------------------------------------------------------

/// Build a worker-friendly config: relaxed per-asset limits plus an enabled
/// automation block with a 1-second cadence.
fn automation_enabled_config() -> AppConfig {
    let mut config = relaxed_default_config();
    config.runtime.automation.enabled = true;
    config.runtime.automation.rebalance_interval_seconds = 1;
    config.runtime.automation.gateway_refill_interval_seconds = 1;
    config.runtime.automation.native_top_up_interval_seconds = 1;
    config.runtime.automation.excess_deposit_interval_seconds = 1;
    config
}

/// Bootstrap a fake-adapter orchestrator with the supplied config and adapters,
/// then spawn the always-on automation workers around it. Returns the
/// orchestrator handle plus the shutdown notifier so tests can stop the
/// workers between assertions.
async fn automation_harness(
    config: AppConfig,
    price_provider: FakePriceProvider,
    htlc_client: FakeHtlcClient,
    swap_executor: FakeSwapExecutor,
    gateway_client: FakeGatewayClient,
    balance_reader: FakeBalanceReader,
) -> (Arc<RuntimeOrchestrator>, Arc<tokio::sync::Notify>) {
    let app_state = bootstrap(config.clone()).await.expect("bootstrap");
    let orchestrator = Arc::new(RuntimeOrchestrator::new(
        app_state,
        RuntimeAdapters {
            price_provider: Arc::new(price_provider),
            htlc_client: Arc::new(htlc_client),
            swap_executor: Arc::new(swap_executor),
            gateway_client: Arc::new(gateway_client),
            balance_reader: Arc::new(balance_reader),
        },
        RuntimeOrchestratorOptions::default(),
    ));
    let shutdown = Arc::new(tokio::sync::Notify::new());
    if config.runtime.automation.enabled {
        firmament::runtime::automation::spawn_workers(
            Arc::clone(&orchestrator),
            &config.runtime.automation,
            Arc::clone(&shutdown),
        );
    }
    (orchestrator, shutdown)
}

/// Inventory snapshot with SOL well above the gas buffer but USDC = $1 — short
/// of the 60% target on a $3-equivalent book — so `decide_rebalance` will plan
/// a SOL → USDC swap.
fn rebalance_drift_inventory() -> BalanceSnapshot {
    balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(1_000_000)),
        TokenAmount::new(sol(), AmountRaw::new(100_000_000)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ])
}

/// Inventory snapshot with USDC well below the Gateway refill threshold and
/// SOL above the gas buffer.
fn gateway_refill_low_usdc_inventory() -> BalanceSnapshot {
    balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(500_000)),
        TokenAmount::new(sol(), AmountRaw::new(100_000_000)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ])
}

/// Inventory snapshot with USDC and SOL at the gas buffer floor — triggers
/// the scoped native SOL top-up decision.
fn native_top_up_low_sol_inventory() -> BalanceSnapshot {
    balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(50_000_000)),
        TokenAmount::new(sol(), AmountRaw::new(10_000_000)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ])
}

fn native_top_up_low_sol_zero_usdc_inventory() -> BalanceSnapshot {
    balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(0)),
        TokenAmount::new(sol(), AmountRaw::new(10_000_000)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ])
}

fn excess_deposit_config() -> AppConfig {
    let mut config = relaxed_default_config();
    config.gateway.usdc_excess_deposit_threshold_raw = AmountRaw::new(12_000_000);
    config.gateway.usdc_excess_deposit_target_raw = AmountRaw::new(10_000_000);
    config
}

fn excess_working_usdc_inventory() -> BalanceSnapshot {
    balance_snapshot(vec![
        TokenAmount::new(usdc(), AmountRaw::new(15_000_000)),
        TokenAmount::new(sol(), AmountRaw::new(100_000_000)),
        TokenAmount::new(cbbtc(), AmountRaw::new(0)),
    ])
}

#[tokio::test]
async fn excess_deposit_fires_when_working_usdc_above_threshold() {
    let gateway = FakeGatewayClient::default();
    let orchestrator = persistent_harness_with_config(
        excess_deposit_config(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        gateway.clone(),
        FakeBalanceReader::new(vec![excess_working_usdc_inventory()]),
        RuntimeOrchestratorOptions::default(),
        Arc::new(
            RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
        ),
    )
    .await;

    let summary = orchestrator
        .run_excess_deposit_check()
        .await
        .expect("excess deposit check");

    assert_eq!(summary.completed_gateway_deposits, 1);
    assert_eq!(gateway.deposit_count(), 1);
    assert_eq!(gateway.deposit_amounts(), vec![AmountRaw::new(5_000_000)]);

    let events = orchestrator.runtime().recent_events(None).await;
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Gateway(GatewayEvent::DepositSubmitted { .. })
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Gateway(GatewayEvent::DepositConfirmed { .. })
    )));
}

#[tokio::test]
async fn excess_deposit_emits_full_ledger_lifecycle() {
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &usdc(), 15_000_000);
    let orchestrator = persistent_harness_with_config(
        excess_deposit_config(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![excess_working_usdc_inventory()]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    orchestrator
        .run_excess_deposit_check()
        .await
        .expect("excess deposit check");

    assert_eq!(
        persistence
            .account_balance(&LedgerAccountId::working(usdc()))
            .expect("working balance"),
        10_000_000
    );
    assert_eq!(
        persistence
            .account_balance(&LedgerAccountId::pending_gateway_deposit(usdc()))
            .expect("pending gateway balance"),
        0
    );
    assert_eq!(
        persistence
            .account_balance(&LedgerAccountId::gateway(usdc()))
            .expect("gateway balance"),
        5_000_000
    );
}

#[tokio::test]
async fn excess_deposit_failure_unwinds_pending() {
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    seed_working_custody(&persistence, &usdc(), 15_000_000);
    let gateway = FakeGatewayClient::with_deposit_failure();
    let orchestrator = persistent_harness_with_config(
        excess_deposit_config(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        gateway.clone(),
        FakeBalanceReader::new(vec![excess_working_usdc_inventory()]),
        RuntimeOrchestratorOptions::default(),
        Arc::clone(&persistence),
    )
    .await;

    let summary = orchestrator
        .run_excess_deposit_check()
        .await
        .expect("excess deposit check records failure without failing tick");

    assert_eq!(summary.completed_gateway_deposits, 0);
    assert_eq!(gateway.deposit_count(), 1);
    assert_eq!(
        persistence
            .account_balance(&LedgerAccountId::working(usdc()))
            .expect("working balance"),
        15_000_000
    );
    assert_eq!(
        persistence
            .account_balance(&LedgerAccountId::pending_gateway_deposit(usdc()))
            .expect("pending gateway balance"),
        0
    );

    let events = orchestrator.runtime().recent_events(None).await;
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Gateway(GatewayEvent::DepositFailed { .. })
    )));
}

#[tokio::test]
async fn rebalance_worker_fires_periodically_when_enabled() {
    let swap = FakeSwapExecutor::default();
    let (orchestrator, shutdown) = automation_harness(
        automation_enabled_config(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        swap.clone(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![rebalance_drift_inventory()]),
    )
    .await;

    // Allow a couple of ticks to fire (1s cadence + slack).
    tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
    shutdown.notify_waiters();

    let events = orchestrator.runtime().recent_events(None).await;
    let rebalance_ticks = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                RuntimeEvent::Automation(AutomationEvent::Tick {
                    kind: AutomationKind::Rebalance,
                    ..
                })
            )
        })
        .count();
    assert!(
        rebalance_ticks >= 1,
        "expected at least one rebalance Automation::Tick, got {rebalance_ticks}"
    );

    let submitted = events.iter().any(|event| {
        matches!(
            event,
            RuntimeEvent::Automation(AutomationEvent::Tick {
                kind: AutomationKind::Rebalance,
                outcome: AutomationOutcome::Submitted { .. },
                ..
            })
        )
    });
    assert!(
        submitted,
        "expected at least one rebalance tick with Submitted outcome",
    );
    assert!(
        swap.executed_count() >= 1,
        "expected swap executor to be called at least once"
    );
}

#[tokio::test]
async fn excess_deposit_worker_fires_when_working_usdc_above_threshold() {
    let gateway = FakeGatewayClient::default();
    let (orchestrator, shutdown) = automation_harness(
        automation_enabled_config(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        gateway.clone(),
        FakeBalanceReader::new(vec![excess_working_usdc_inventory()]),
    )
    .await;

    tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
    shutdown.notify_waiters();

    assert!(
        gateway.deposit_count() >= 1,
        "expected excess deposit worker to call Gateway deposit"
    );

    let events = orchestrator.runtime().recent_events(None).await;
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Automation(AutomationEvent::Tick {
            kind: AutomationKind::ExcessDeposit,
            outcome: AutomationOutcome::Submitted { .. },
            ..
        })
    )));
}

#[tokio::test]
async fn rebalance_worker_does_not_fire_when_disabled() {
    let swap = FakeSwapExecutor::default();
    // Default automation config keeps `enabled = false`.
    let (orchestrator, shutdown) = automation_harness(
        relaxed_default_config(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        swap.clone(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![rebalance_drift_inventory()]),
    )
    .await;

    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    shutdown.notify_waiters();

    let events = orchestrator.runtime().recent_events(None).await;
    let any_automation_tick = events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Automation(_)));
    assert!(
        !any_automation_tick,
        "expected no Automation events when disabled"
    );
    assert_eq!(
        swap.executed_count(),
        0,
        "expected swap executor untouched when automation is disabled"
    );
}

#[tokio::test]
async fn gateway_refill_worker_fires_when_working_custody_below_threshold() {
    let gateway = FakeGatewayClient::default();
    let (orchestrator, shutdown) = automation_harness(
        automation_enabled_config(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        FakeSwapExecutor::default(),
        gateway.clone(),
        FakeBalanceReader::new(vec![gateway_refill_low_usdc_inventory()]),
    )
    .await;

    tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
    shutdown.notify_waiters();

    let events = orchestrator.runtime().recent_events(None).await;
    let gateway_ticks = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                RuntimeEvent::Automation(AutomationEvent::Tick {
                    kind: AutomationKind::GatewayRefill,
                    ..
                })
            )
        })
        .count();
    assert!(
        gateway_ticks >= 1,
        "expected at least one gateway_refill tick, got {gateway_ticks}"
    );
    assert!(
        gateway.refill_count() >= 1,
        "expected gateway refill to be requested at least once"
    );
}

#[tokio::test]
async fn native_top_up_worker_fires_when_native_sol_low() {
    let swap = FakeSwapExecutor::default();
    // The native top-up branch in `decide_rebalance` requires a non-zero USD
    // price for SOL so the plan does not collapse to ZeroAmount. Drive an
    // initial RFQ to seed the orchestrator's USD price book before letting
    // the workers tick.
    let (orchestrator, shutdown) = automation_harness(
        automation_enabled_config(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        swap.clone(),
        FakeGatewayClient::default(),
        FakeBalanceReader::new(vec![
            quote_inventory(),
            native_top_up_low_sol_inventory(),
            native_top_up_low_sol_inventory(),
        ]),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq(1_000_000))
        .await
        .expect("seed price book via RFQ");
    assert!(matches!(response, RfqResponse::Accepted(_)));

    tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
    shutdown.notify_waiters();

    let events = orchestrator.runtime().recent_events(None).await;
    let native_ticks = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                RuntimeEvent::Automation(AutomationEvent::Tick {
                    kind: AutomationKind::NativeTopUp,
                    ..
                })
            )
        })
        .count();
    assert!(
        native_ticks >= 1,
        "expected at least one native_top_up tick, got {native_ticks}"
    );
    let usdc_to_sol_swap = swap.quoted_requests().iter().any(|request| {
        request.pair == AssetPair::new(usdc(), sol())
            || request.pair == AssetPair::new(sol(), usdc())
    });
    assert!(
        usdc_to_sol_swap || swap.executed_count() >= 1,
        "expected the native top-up flow to invoke the swap executor"
    );
}

#[tokio::test]
async fn native_top_up_chains_gateway_refill_when_working_usdc_short() {
    let gateway = FakeGatewayClient::default();
    let swap = FakeSwapExecutor::default();
    let orchestrator = harness(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        swap.clone(),
        gateway.clone(),
        FakeBalanceReader::new(vec![
            quote_inventory(),
            native_top_up_low_sol_zero_usdc_inventory(),
        ]),
        RuntimeOrchestratorOptions::default(),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq(1_000_000))
        .await
        .expect("seed price book via RFQ");
    assert!(matches!(response, RfqResponse::Accepted(_)));

    orchestrator
        .run_native_top_up_check()
        .await
        .expect("native top-up check");

    assert_eq!(gateway.refill_count(), 1);
    assert_eq!(
        gateway.refill_requests()[0].amount.amount_raw,
        AmountRaw::new(1_000_000)
    );
    assert_eq!(swap.executed_count(), 1);
}

#[tokio::test]
async fn native_top_up_skips_gateway_when_working_usdc_sufficient() {
    let gateway = FakeGatewayClient::default();
    let swap = FakeSwapExecutor::default();
    let orchestrator = harness(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        swap.clone(),
        gateway.clone(),
        FakeBalanceReader::new(vec![quote_inventory(), native_top_up_low_sol_inventory()]),
        RuntimeOrchestratorOptions::default(),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq(1_000_000))
        .await
        .expect("seed price book via RFQ");
    assert!(matches!(response, RfqResponse::Accepted(_)));

    orchestrator
        .run_native_top_up_check()
        .await
        .expect("native top-up check");

    assert_eq!(gateway.refill_count(), 0);
    assert_eq!(swap.executed_count(), 1);
}

#[tokio::test]
async fn native_top_up_failure_in_gateway_skips_swap() {
    let gateway = FakeGatewayClient::with_refill_failure();
    let swap = FakeSwapExecutor::default();
    let orchestrator = harness(
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        swap.clone(),
        gateway.clone(),
        FakeBalanceReader::new(vec![
            quote_inventory(),
            native_top_up_low_sol_zero_usdc_inventory(),
        ]),
        RuntimeOrchestratorOptions::default(),
    )
    .await;

    let response = orchestrator
        .request_rfq(rfq(1_000_000))
        .await
        .expect("seed price book via RFQ");
    assert!(matches!(response, RfqResponse::Accepted(_)));

    orchestrator
        .run_native_top_up_check()
        .await
        .expect("native top-up check records gateway failure");

    assert_eq!(gateway.refill_count(), 1);
    assert_eq!(swap.executed_count(), 0);
    let events = orchestrator.runtime().recent_events(None).await;
    assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Gateway(GatewayEvent::RefillFailed { amount, reason, .. })
            if amount.asset == usdc()
                && amount.amount_raw == AmountRaw::new(1_000_000)
                && reason.contains("fake gateway refill failed")
    )));
}

#[tokio::test]
async fn workers_do_not_race_with_trade_driven_automation() {
    // Drive a trade-driven `accept_quote` (which calls
    // refresh_inventory_and_automation under the same automation_lock)
    // while the workers are actively ticking. The shared mutex means
    // automation runs cannot interleave; cumulative-cap accounting and
    // adapter calls stay deterministic.
    let swap = FakeSwapExecutor::default();
    let gateway = FakeGatewayClient::default();
    let (orchestrator, shutdown) = automation_harness(
        automation_enabled_config(),
        FakePriceProvider::default(),
        FakeHtlcClient::default(),
        swap.clone(),
        gateway.clone(),
        FakeBalanceReader::new(vec![quote_inventory()]),
    )
    .await;

    let quote_id = accepted_quote_id(orchestrator.request_rfq(rfq(1_000_000)).await);
    orchestrator
        .accept_quote(quote_id)
        .await
        .expect("trade-driven accept_quote should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
    shutdown.notify_waiters();

    // Cumulative cap defaults to $15 and the per-action cap to $2, so we
    // expect at most a small bounded number of swap submissions.
    assert!(
        swap.executed_count() <= 8,
        "automation lock should serialize, got {} executed swaps",
        swap.executed_count()
    );
    assert!(
        gateway.refill_count() <= 8,
        "automation lock should serialize, got {} refills",
        gateway.refill_count()
    );

    let events = orchestrator.runtime().recent_events(None).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Automation(AutomationEvent::Tick { .. })
        )),
        "expected at least one Automation::Tick from the workers"
    );
}
