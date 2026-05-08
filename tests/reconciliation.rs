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
use firmament::runtime::reconciliation::{ObservationOutcome, ReconciliationWorker};
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
    #[allow(dead_code)]
    gateway_client: ProgrammableGatewayClient,
}

async fn build_harness() -> Harness {
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(AssetRegistry::default())
            .expect("open in-memory persistence"),
    );
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
