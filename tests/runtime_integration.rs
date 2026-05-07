use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use firmament::assets::AssetRegistry;
use firmament::db::Db;
use firmament::events::{
    EventMetadata, InventoryEvent, QuoteEvent, RuntimeEvent, SettlementEvent, SwapEvent,
};
use firmament::ledger::{LedgerAccountId, LedgerEventConsumer, SqliteLedgerRepository};
use firmament::ports::{BalanceReader, GatewayClient, HtlcClient, PriceProvider, SwapExecutor};
use firmament::rfq::{RfqRequest, RfqResponse};
use firmament::settlement::SettlementLeg;
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
        AppConfig::default(),
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
    let app_state = bootstrap(config)
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
    .debit(LedgerAccountId::working(asset.clone()), AmountRaw::new(amount))
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
    .credit(LedgerAccountId::working(asset.clone()), AmountRaw::new(amount))
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
    let transaction = firmament::ledger::LedgerTransactionBuilder::new(
        "test_seed_gateway",
        uuid::Uuid::now_v7(),
    )
    .description("seed gateway USDC for Gateway-quoteability test")
    .idempotency_key(format!(
        "test:seed:gateway:{}:{}:{}",
        asset.as_str(),
        amount,
        uuid::Uuid::now_v7(),
    ))
    .debit(LedgerAccountId::gateway(asset.clone()), AmountRaw::new(amount))
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
    // them to $1000 to make the gate the load-bearing assertion.
    let mut config = AppConfig::default();
    config.risk.max_quote_notional_usd = Decimal::from(1_000);
    config.risk.max_trade_notional_usd = Decimal::from(1_000);
    config.risk.max_daily_notional_usd = Decimal::from(10_000);
    config.risk.max_non_stable_asset_notional_usd = Decimal::from(1_000);

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
        WalletRole::Taker | WalletRole::Operator | WalletRole::Gateway => {
            SettlementLeg::TakerInput
        }
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
                AppError::validation(format!(
                    "fake htlc client: unknown HTLC trade {trade_id}"
                ))
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
        signature: Some(TxSignature::new(format!(
            "{leg:?}-{trade_id}-{status:?}"
        ))),
    }
}

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
        .debit(LedgerAccountId::working(sol()), AmountRaw::new(maker_output_amount))
        .credit(
            LedgerAccountId::external(sol(), "seed"),
            AmountRaw::new(maker_output_amount),
        )
        .build()
        .expect("balanced seed");
    repository
        .save_transaction(&seed)
        .expect("save seed");

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

    let seed = firmament::ledger::LedgerTransactionBuilder::new(
        "test_seed",
        trade_id.as_uuid(),
    )
    .description("seed maker SOL working custody")
    .idempotency_key(format!("seed:{trade_id}:working_sol"))
    .debit(LedgerAccountId::working(sol()), AmountRaw::new(maker_output_amount))
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
