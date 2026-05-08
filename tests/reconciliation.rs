//! Tests for the always-on reconciliation worker.
//!
//! These tests exercise [`firmament::runtime::reconciliation`] end-to-end
//! against fake `BalanceReader` and `GatewayClient` adapters that return
//! programmable drift sequences.

#![allow(clippy::too_many_lines)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use firmament::assets::AssetRegistry;
use firmament::config::ReconciliationConfig;
use firmament::events::{ReconciliationEvent, ReconciliationOutcome, RuntimeEvent};
use firmament::ledger::{LedgerAccountId, LedgerTransactionBuilder};
use firmament::ports::{BalanceReader, GatewayClient, HtlcClient, PriceProvider, SwapExecutor};
use firmament::runtime::reconciliation::{ObservationOutcome, ReconciliationWorker, today_utc_date};
use firmament::runtime::{
    RuntimeAdapters, RuntimeOrchestrator, RuntimeOrchestratorOptions, RuntimePersistence,
};
use firmament::types::{
    AmountRaw, AssetId, AssetPair, BalanceSnapshot, GatewayReceipt, GatewayRefillRequest,
    HtlcInitiation, HtlcReceipt, ReferencePrice, SettlementStatus, SwapQuote, SwapReceipt,
    SwapRequest, TokenAmount, TradeId, WalletRole,
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

fn balance_snapshot(balances: Vec<TokenAmount>) -> BalanceSnapshot {
    BalanceSnapshot {
        wallet: WalletRole::Maker,
        balances,
        observed_at: OffsetDateTime::now_utc(),
    }
}

/// Programmable balance reader. `set` controls the per-asset raw amount
/// returned by every subsequent `balances` call.
#[derive(Debug, Clone, Default)]
struct ProgrammableBalanceReader {
    inner: Arc<Mutex<HashMap<AssetId, u64>>>,
}

impl ProgrammableBalanceReader {
    fn new() -> Self {
        Self::default()
    }

    fn set(&self, asset: AssetId, raw: u64) {
        self.inner
            .lock()
            .expect("programmable balance lock")
            .insert(asset, raw);
    }
}

#[async_trait]
impl BalanceReader for ProgrammableBalanceReader {
    async fn balances(&self, _wallet: WalletRole) -> Result<BalanceSnapshot, AppError> {
        let map = self.inner.lock().expect("programmable balance lock");
        let balances = map
            .iter()
            .map(|(asset, raw)| TokenAmount::new(asset.clone(), AmountRaw::new(*raw)))
            .collect();
        Ok(balance_snapshot(balances))
    }
}

/// Programmable Gateway client returning a mutable USDC balance.
#[derive(Debug, Clone)]
struct ProgrammableGatewayClient {
    balance_raw: Arc<Mutex<u64>>,
}

impl ProgrammableGatewayClient {
    fn new(initial: u64) -> Self {
        Self {
            balance_raw: Arc::new(Mutex::new(initial)),
        }
    }

    fn set(&self, raw: u64) {
        *self.balance_raw.lock().expect("gateway lock") = raw;
    }
}

#[async_trait]
impl GatewayClient for ProgrammableGatewayClient {
    async fn balance(&self, asset: AssetId) -> Result<GatewayReceipt, AppError> {
        let raw = *self.balance_raw.lock().expect("gateway lock");
        Ok(GatewayReceipt {
            amount: TokenAmount::new(asset, AmountRaw::new(raw)),
            provider_transfer_id: None,
            signature: None,
        })
    }

    async fn request_refill(
        &self,
        request: GatewayRefillRequest,
    ) -> Result<GatewayReceipt, AppError> {
        Ok(GatewayReceipt {
            amount: request.amount,
            provider_transfer_id: None,
            signature: None,
        })
    }
}

#[derive(Debug, Clone, Default)]
struct InertHtlcClient;

#[async_trait]
impl HtlcClient for InertHtlcClient {
    async fn initiate(&self, _request: HtlcInitiation) -> Result<HtlcReceipt, AppError> {
        Err(AppError::unsupported("inert htlc client"))
    }

    async fn redeem(&self, _trade_id: TradeId, _preimage: String) -> Result<HtlcReceipt, AppError> {
        Err(AppError::unsupported("inert htlc client"))
    }

    async fn refund(&self, _trade_id: TradeId) -> Result<HtlcReceipt, AppError> {
        Err(AppError::unsupported("inert htlc client"))
    }

    async fn status(&self, _trade_id: TradeId) -> Result<SettlementStatus, AppError> {
        Ok(SettlementStatus::Pending)
    }
}

#[derive(Debug, Clone, Default)]
struct InertSwapExecutor;

#[async_trait]
impl SwapExecutor for InertSwapExecutor {
    async fn quote_swap(&self, _request: SwapRequest) -> Result<SwapQuote, AppError> {
        Err(AppError::unsupported("inert swap executor"))
    }

    async fn execute_swap(&self, _quote: SwapQuote) -> Result<SwapReceipt, AppError> {
        Err(AppError::unsupported("inert swap executor"))
    }
}

#[derive(Debug, Clone, Default)]
struct InertPriceProvider;

#[async_trait]
impl PriceProvider for InertPriceProvider {
    async fn reference_price(&self, pair: AssetPair) -> Result<ReferencePrice, AppError> {
        Ok(ReferencePrice {
            pair,
            output_per_input: Decimal::ONE,
            observed_at: OffsetDateTime::now_utc(),
        })
    }
}

struct Harness {
    orchestrator: Arc<RuntimeOrchestrator>,
    persistence: Arc<RuntimePersistence>,
    balance_reader: ProgrammableBalanceReader,
    gateway_client: ProgrammableGatewayClient,
}

async fn build_harness() -> Harness {
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default())
            .expect("open in-memory persistence"),
    );
    build_harness_with_persistence(persistence).await
}

async fn build_harness_with_persistence(persistence: Arc<RuntimePersistence>) -> Harness {
    let app_state = bootstrap(AppConfig::default())
        .await
        .expect("bootstrap runtime state");
    let balance_reader = ProgrammableBalanceReader::new();
    let gateway_client = ProgrammableGatewayClient::new(0);
    let orchestrator = Arc::new(RuntimeOrchestrator::new_with_persistence(
        app_state,
        RuntimeAdapters {
            price_provider: Arc::new(InertPriceProvider),
            htlc_client: Arc::new(InertHtlcClient),
            swap_executor: Arc::new(InertSwapExecutor),
            gateway_client: Arc::new(gateway_client.clone()),
            balance_reader: Arc::new(balance_reader.clone()),
        },
        Arc::clone(&persistence),
        RuntimeOrchestratorOptions::default(),
    ));
    Harness {
        orchestrator,
        persistence,
        balance_reader,
        gateway_client,
    }
}

fn seed_gateway(persistence: &Arc<RuntimePersistence>, asset: &AssetId, amount: u64) {
    if amount == 0 {
        return;
    }
    let transaction = LedgerTransactionBuilder::new("test_seed_gateway", uuid::Uuid::now_v7())
        .description("seed gateway for reconciliation test")
        .idempotency_key(format!(
            "test:seed:gateway:{}:{}:{}",
            asset.as_str(),
            amount,
            uuid::Uuid::now_v7()
        ))
        .debit(LedgerAccountId::gateway(asset.clone()), AmountRaw::new(amount))
        .credit(
            LedgerAccountId::external(asset.clone(), "seed"),
            AmountRaw::new(amount),
        )
        .build()
        .expect("balanced gateway seed");
    persistence
        .save_ledger_transaction(&transaction)
        .expect("save gateway seed");
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
    let transaction =
        LedgerTransactionBuilder::new("test_seed_gateway_reserved", uuid::Uuid::now_v7())
            .description("seed gateway_reserved")
            .idempotency_key(format!(
                "test:seed:gateway_reserved:{}:{}:{}:{}",
                asset.as_str(),
                trade_id,
                amount,
                uuid::Uuid::now_v7()
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
            .expect("balanced gateway_reserved seed");
    persistence
        .save_ledger_transaction(&transaction)
        .expect("save gateway_reserved seed");
}

fn seed_in_flight_pending_escrow(
    persistence: &Arc<RuntimePersistence>,
    asset: &AssetId,
    amount: u64,
) {
    if amount == 0 {
        return;
    }
    let transaction =
        LedgerTransactionBuilder::new("test_seed_pending_escrow", uuid::Uuid::now_v7())
            .description("seed pending_escrow for in-flight trade")
            .idempotency_key(format!(
                "test:seed:pending_escrow:{}:{}:{}",
                asset.as_str(),
                amount,
                uuid::Uuid::now_v7()
            ))
            .debit(
                LedgerAccountId::pending_escrow(asset.clone()),
                AmountRaw::new(amount),
            )
            .credit(
                LedgerAccountId::external(asset.clone(), "seed_in_flight"),
                AmountRaw::new(amount),
            )
            .build()
            .expect("balanced pending_escrow seed");
    persistence
        .save_ledger_transaction(&transaction)
        .expect("save pending_escrow seed");
}

fn seed_working_custody(persistence: &Arc<RuntimePersistence>, asset: &AssetId, amount: u64) {
    if amount == 0 {
        return;
    }
    let transaction = LedgerTransactionBuilder::new("test_seed_working", uuid::Uuid::now_v7())
        .description("seed working custody for reconciliation test")
        .idempotency_key(format!(
            "test:seed:working:{}:{}:{}",
            asset.as_str(),
            amount,
            uuid::Uuid::now_v7()
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
        .expect("save seed transaction");
}

fn build_worker(harness: &Harness) -> ReconciliationWorker {
    ReconciliationWorker::new(
        Arc::clone(&harness.orchestrator),
        ReconciliationConfig::default(),
    )
    .expect("build reconciliation worker")
}

async fn recent_recon_events(harness: &Harness) -> Vec<ReconciliationEvent> {
    harness
        .orchestrator
        .runtime()
        .recent_events(None)
        .await
        .into_iter()
        .filter_map(|event| match event {
            RuntimeEvent::Reconciliation(recon) => Some(recon),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn recon_wallet_positive_drift_adjusts_after_three_consecutive_ticks() {
    let harness = build_harness().await;

    // Ledger working_custody:USDC = 1_000_000 (above dust 10_000).
    seed_working_custody(&harness.persistence, &usdc(), 1_000_000);
    // On-chain reads as 1_020_000 → drift = +20_000.
    harness.balance_reader.set(usdc(), 1_020_000);
    // Other assets present so the wallet snapshot is well-formed.
    harness.balance_reader.set(sol(), 0);
    harness.balance_reader.set(cbbtc(), 0);

    let mut worker = build_worker(&harness);

    let observation = worker
        .tick_wallet(&usdc())
        .await
        .expect("tick 1 must succeed");
    assert!(matches!(
        observation.outcome,
        ObservationOutcome::Building { observations: 1 }
    ));

    let observation = worker
        .tick_wallet(&usdc())
        .await
        .expect("tick 2 must succeed");
    assert!(matches!(
        observation.outcome,
        ObservationOutcome::Building { observations: 2 }
    ));

    let observation = worker
        .tick_wallet(&usdc())
        .await
        .expect("tick 3 must succeed");
    let key = match &observation.outcome {
        ObservationOutcome::Adjusted { idempotency_key } => idempotency_key.clone(),
        other => panic!("expected Adjusted, got {other:?}"),
    };
    assert!(
        key.starts_with("recon:wallet:USDC:"),
        "unexpected idempotency key: {key}"
    );
    assert!(key.ends_with(":1"), "first adjustment must use seq=1: {key}");

    let working = harness
        .persistence
        .account_balance(&LedgerAccountId::working(usdc()))
        .expect("working balance");
    assert_eq!(working, 1_020_000);

    let external = harness
        .persistence
        .account_balance(&LedgerAccountId::external(usdc(), "reconciliation"))
        .expect("external recon balance");
    assert_eq!(external, -20_000);

    let events = recent_recon_events(&harness).await;
    let last = events.last().expect("at least one recon event");
    let ReconciliationEvent::Tick { outcome, .. } = last;
    assert!(
        matches!(
            outcome,
            ReconciliationOutcome::Adjusted { idempotency_key } if idempotency_key == &key
        ),
        "expected Adjusted event with key {key}, got {outcome:?}",
    );
}

#[tokio::test]
async fn recon_wallet_negative_drift_adjusts() {
    let harness = build_harness().await;

    seed_working_custody(&harness.persistence, &usdc(), 1_000_000);
    harness.balance_reader.set(usdc(), 980_000); // drift = -20_000
    harness.balance_reader.set(sol(), 0);
    harness.balance_reader.set(cbbtc(), 0);

    let mut worker = build_worker(&harness);
    for _ in 0..2 {
        worker.tick_wallet(&usdc()).await.expect("tick");
    }
    let final_obs = worker.tick_wallet(&usdc()).await.expect("final tick");
    let key = match final_obs.outcome {
        ObservationOutcome::Adjusted { idempotency_key } => idempotency_key,
        other => panic!("expected Adjusted, got {other:?}"),
    };
    assert!(key.starts_with("recon:wallet:USDC:"));

    let working = harness
        .persistence
        .account_balance(&LedgerAccountId::working(usdc()))
        .expect("working");
    assert_eq!(working, 980_000);
    let external = harness
        .persistence
        .account_balance(&LedgerAccountId::external(usdc(), "reconciliation"))
        .expect("external");
    assert_eq!(external, 20_000);
}

#[tokio::test]
async fn recon_dust_threshold_skips() {
    let harness = build_harness().await;

    seed_working_custody(&harness.persistence, &usdc(), 1_000_000);
    harness.balance_reader.set(usdc(), 1_005_000); // drift = +5_000 (dust=10_000)
    harness.balance_reader.set(sol(), 0);
    harness.balance_reader.set(cbbtc(), 0);

    let mut worker = build_worker(&harness);
    for _ in 0..3 {
        let observation = worker.tick_wallet(&usdc()).await.expect("tick");
        assert!(matches!(observation.outcome, ObservationOutcome::WithinDust));
    }

    // No adjustment occurred — working_custody stays at the seeded value.
    let working = harness
        .persistence
        .account_balance(&LedgerAccountId::working(usdc()))
        .expect("working");
    assert_eq!(working, 1_000_000);
    let external_recon = harness
        .persistence
        .account_balance(&LedgerAccountId::external(usdc(), "reconciliation"))
        .expect("external");
    assert_eq!(external_recon, 0);
}

#[tokio::test]
async fn recon_sign_flip_resets_window() {
    let harness = build_harness().await;

    seed_working_custody(&harness.persistence, &usdc(), 1_000_000);
    harness.balance_reader.set(sol(), 0);
    harness.balance_reader.set(cbbtc(), 0);

    let mut worker = build_worker(&harness);

    harness.balance_reader.set(usdc(), 1_020_000);
    let obs1 = worker.tick_wallet(&usdc()).await.expect("tick 1");
    assert!(matches!(
        obs1.outcome,
        ObservationOutcome::Building { observations: 1 }
    ));
    let obs2 = worker.tick_wallet(&usdc()).await.expect("tick 2");
    assert!(matches!(
        obs2.outcome,
        ObservationOutcome::Building { observations: 2 }
    ));

    // Sign flip: balance dips below the seeded amount → drift = -20_000.
    harness.balance_reader.set(usdc(), 980_000);
    let obs3 = worker.tick_wallet(&usdc()).await.expect("tick 3");
    assert!(
        matches!(
            obs3.outcome,
            ObservationOutcome::Building { observations: 1 }
        ),
        "sign flip must reset to observations=1, got {obs3:?}"
    );

    let external_recon = harness
        .persistence
        .account_balance(&LedgerAccountId::external(usdc(), "reconciliation"))
        .expect("external");
    assert_eq!(external_recon, 0);
}

#[tokio::test]
async fn recon_in_flight_trade_pauses() {
    let harness = build_harness().await;

    seed_working_custody(&harness.persistence, &usdc(), 1_000_000);
    // Active in-flight pending_escrow:USDC entry simulates a trade in flight.
    seed_in_flight_pending_escrow(&harness.persistence, &usdc(), 50_000);
    harness.balance_reader.set(usdc(), 1_020_000);
    harness.balance_reader.set(sol(), 0);
    harness.balance_reader.set(cbbtc(), 0);

    let mut worker = build_worker(&harness);
    worker.tick_wallet(&usdc()).await.expect("tick 1");
    worker.tick_wallet(&usdc()).await.expect("tick 2");
    let trigger = worker.tick_wallet(&usdc()).await.expect("trigger tick");
    match trigger.outcome {
        ObservationOutcome::Skipped { reason } => assert_eq!(reason, "trade_in_flight"),
        other => panic!("expected Skipped(trade_in_flight), got {other:?}"),
    }

    let external_recon = harness
        .persistence
        .account_balance(&LedgerAccountId::external(usdc(), "reconciliation"))
        .expect("external");
    assert_eq!(external_recon, 0);

    let events = recent_recon_events(&harness).await;
    let last = events.last().expect("recon event present");
    let ReconciliationEvent::Tick { outcome, .. } = last;
    assert!(matches!(
        outcome,
        ReconciliationOutcome::Skipped { reason } if reason == "trade_in_flight"
    ));
}

#[tokio::test]
async fn recon_emits_event_on_skip() {
    // Even a within-dust observation must emit a Tick event so operators
    // can see the worker is alive.
    let harness = build_harness().await;

    seed_working_custody(&harness.persistence, &usdc(), 1_000_000);
    harness.balance_reader.set(usdc(), 1_005_000); // within dust
    harness.balance_reader.set(sol(), 0);
    harness.balance_reader.set(cbbtc(), 0);

    let mut worker = build_worker(&harness);
    worker.tick_wallet(&usdc()).await.expect("tick");

    let events = recent_recon_events(&harness).await;
    let last = events.last().expect("at least one tick event");
    let ReconciliationEvent::Tick {
        outcome, scope, ..
    } = last;
    assert!(matches!(
        scope,
        firmament::events::ReconciliationScope::Wallet
    ));
    assert!(matches!(outcome, ReconciliationOutcome::WithinDust));
}

#[tokio::test]
async fn recon_gateway_positive_drift_adjusts_external_to_gateway() {
    let harness = build_harness().await;

    seed_gateway(&harness.persistence, &usdc(), 1_000_000);
    harness.gateway_client.set(1_020_000); // drift = +20_000

    let mut worker = build_worker(&harness);
    let mut last_outcome: Option<ObservationOutcome> = None;
    for _ in 0..3 {
        last_outcome = Some(
            worker
                .tick_gateway(&usdc())
                .await
                .expect("gateway tick")
                .outcome,
        );
    }
    let key = match last_outcome.expect("three ticks ran") {
        ObservationOutcome::Adjusted { idempotency_key } => idempotency_key,
        other => panic!("expected Adjusted, got {other:?}"),
    };
    assert!(
        key.starts_with("recon:gateway:USDC:"),
        "unexpected gateway key: {key}"
    );

    let gateway = harness
        .persistence
        .account_balance(&LedgerAccountId::gateway(usdc()))
        .expect("gateway balance");
    assert_eq!(gateway, 1_020_000);

    let external = harness
        .persistence
        .account_balance(&LedgerAccountId::external(usdc(), "reconciliation"))
        .expect("external");
    assert_eq!(external, -20_000);
}

#[tokio::test]
async fn recon_gateway_skip_when_gateway_reserved_active() {
    let harness = build_harness().await;

    // Gateway holds 1_000_000 USDC, with 100 reserved for an in-flight trade.
    seed_gateway(&harness.persistence, &usdc(), 1_000_000);
    seed_gateway_reserved(&harness.persistence, &usdc(), "trade-1", 100);

    // On-chain Gateway balance is BELOW expected (1_000_100). Drift is
    // 980_000 - 1_000_100 = -20_100 — well above dust and negative.
    harness.gateway_client.set(980_000);

    let mut worker = build_worker(&harness);
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        outcomes.push(
            worker
                .tick_gateway(&usdc())
                .await
                .expect("gateway tick")
                .outcome,
        );
    }

    let last = outcomes.last().expect("three ticks ran").clone();
    match last {
        ObservationOutcome::Skipped { reason } => {
            assert_eq!(reason, "gateway_reserved_active");
        }
        other => panic!("expected Skipped(gateway_reserved_active), got {other:?}"),
    }

    let external = harness
        .persistence
        .account_balance(&LedgerAccountId::external(usdc(), "reconciliation"))
        .expect("external");
    assert_eq!(external, 0);
}

#[tokio::test]
async fn recon_idempotent_across_restart() {
    // Tick three times to post one adjustment with sequence 1; then drop
    // the worker, construct a fresh worker against the same persistence,
    // bump the chain again, and tick three more times. The new worker
    // must reseed its sequence map from the ledger so the next adjustment
    // lands at sequence 2 — never duplicating sequence 1.

    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default()).expect("persistence"),
    );
    let harness = build_harness_with_persistence(Arc::clone(&persistence)).await;

    seed_working_custody(&harness.persistence, &usdc(), 1_000_000);
    harness.balance_reader.set(usdc(), 1_020_000);
    harness.balance_reader.set(sol(), 0);
    harness.balance_reader.set(cbbtc(), 0);

    let mut worker = build_worker(&harness);
    for _ in 0..3 {
        worker.tick_wallet(&usdc()).await.expect("tick");
    }

    let count_after_first =
        count_recon_idempotency_keys(&persistence, "recon:wallet:USDC");
    assert_eq!(count_after_first, 1);

    // Restart: drop the in-memory worker and build a fresh one against the
    // same persistence. The reseed should recover seq=1.
    drop(worker);
    let mut worker = build_worker(&harness);

    // Push the chain higher again to create a fresh +20_000 drift
    // relative to the new working_custody (which is now 1_020_000).
    harness.balance_reader.set(usdc(), 1_040_000);
    for _ in 0..3 {
        worker
            .tick_wallet(&usdc())
            .await
            .expect("tick after restart");
    }

    let count_after_restart =
        count_recon_idempotency_keys(&persistence, "recon:wallet:USDC");
    assert_eq!(
        count_after_restart, 2,
        "restart must not duplicate adjustments — expected exactly 2 distinct recon keys"
    );

    let date = today_utc_date();
    let prefix = format!("recon:wallet:USDC:{date}:");
    let max_seq = max_seq_for_prefix(&persistence, &prefix);
    assert_eq!(max_seq, Some(2));
}

fn count_recon_idempotency_keys(persistence: &Arc<RuntimePersistence>, prefix: &str) -> usize {
    persistence
        .with_recon_idempotency_keys(prefix, |keys| keys.len())
        .expect("read recon keys")
}

fn max_seq_for_prefix(persistence: &Arc<RuntimePersistence>, prefix: &str) -> Option<u64> {
    persistence
        .with_recon_idempotency_keys(prefix, |keys| {
            keys.iter()
                .filter_map(|key| key.strip_prefix(prefix))
                .filter_map(|seq| seq.parse::<u64>().ok())
                .max()
        })
        .expect("read recon keys")
}
