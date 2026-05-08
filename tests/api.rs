use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use firmament::ports::{BalanceReader, GatewayClient, HtlcClient, PriceProvider, SwapExecutor};
use firmament::runtime::{
    RuntimeAdapters, RuntimeOrchestrator, RuntimeOrchestratorOptions, RuntimePersistence,
};
use firmament::settlement::SettlementLeg;
use firmament::types::{
    AmountRaw, AssetId, AssetPair, BalanceSnapshot, ExternalHtlcInitiation, GatewayReceipt,
    GatewayRefillRequest, HtlcInitiation, HtlcReceipt, ReferencePrice, SettlementStatus, SwapQuote,
    SwapReceipt, SwapRequest, TokenAmount, TxSignature, UnsignedWalletTransaction, WalletAddress,
    WalletRole,
};
use firmament::{AppConfig, api, bootstrap};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use tower::ServiceExt;

async fn test_router() -> axum::Router {
    let app_state = bootstrap(AppConfig::default())
        .await
        .expect("bootstrap state");
    api::router(&app_state)
}

async fn test_orchestrator_router() -> axum::Router {
    let app_state = bootstrap(AppConfig::default())
        .await
        .expect("bootstrap state");
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(firmament::assets::AssetRegistry::default())
            .expect("persistence"),
    );
    // RFQ inventory now reads from ledger working_custody. Seed the ledger
    // with matching maker SOL/USDC so legacy API tests still observe an
    // accepted quote on the same inputs.
    seed_working_custody(&persistence, AssetId::from("SOL"), 200_000_000);
    seed_working_custody(&persistence, AssetId::from("USDC"), 10_000_000);
    let orchestrator = RuntimeOrchestrator::new_with_persistence(
        app_state,
        RuntimeAdapters {
            price_provider: Arc::new(FakePriceProvider::default()),
            htlc_client: Arc::new(FakeHtlcClient::default()),
            swap_executor: Arc::new(FakeSwapExecutor),
            gateway_client: Arc::new(FakeGatewayClient),
            balance_reader: Arc::new(FakeBalanceReader::new(vec![
                quote_inventory(),
                post_settlement_inventory(),
            ])),
        },
        persistence,
        RuntimeOrchestratorOptions::default(),
    );
    api::router_with_orchestrator(Arc::new(orchestrator))
}

fn seed_working_custody(persistence: &Arc<RuntimePersistence>, asset: AssetId, amount: u64) {
    if amount == 0 {
        return;
    }
    let transaction = firmament::ledger::LedgerTransactionBuilder::new(
        "test_seed_working_custody",
        uuid::Uuid::now_v7(),
    )
    .description("seed working custody for API RFQ test")
    .idempotency_key(format!(
        "test:api:seed:working:{}:{}:{}",
        asset.as_str(),
        amount,
        uuid::Uuid::now_v7(),
    ))
    .debit(
        firmament::ledger::LedgerAccountId::working(asset.clone()),
        AmountRaw::new(amount),
    )
    .credit(
        firmament::ledger::LedgerAccountId::external(asset, "seed"),
        AmountRaw::new(amount),
    )
    .build()
    .expect("balanced seed transaction");
    persistence
        .save_ledger_transaction(&transaction)
        .expect("save seed ledger transaction");
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    serde_json::from_slice(&bytes).expect("response json")
}

fn rfq_request() -> Value {
    json!({
        "input_mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        "output_mint": "So11111111111111111111111111111111111111112",
        "input_amount_raw": 1000,
        "taker_wallet": "DemoTaker111111111111111111111111111111111111",
        "expiry_seconds": 30
    })
}

#[tokio::test]
async fn api_runtime_state_endpoint_success() {
    let app = test_router().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/state")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert!(body["state"]["run_id"].is_string());
    assert_eq!(body["state"]["inventory"]["status"], "runtime_ready");
}

#[tokio::test]
async fn api_runtime_events_endpoint_success() {
    let app = test_router().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/events")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["count"], 1);
    assert_eq!(body["events"][0]["category"], "system");
}

#[tokio::test]
async fn api_legacy_accept_route_requires_wallet_settlement() {
    let app = test_orchestrator_router().await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/rfq")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(rfq_request().to_string()))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let quote_body = response_json(response).await;
    assert_eq!(quote_body["status"], "accepted");
    assert_eq!(quote_body["integration_status"], "runtime_orchestrated");
    assert_ne!(quote_body["quoted_output_amount_raw"], 0);
    assert!(quote_body["expires_at"].is_string());
    assert!(quote_body["htlc_terms"]["expires_at"].is_string());
    let quote_id = quote_body["quote_id"].as_str().expect("quote id");

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/quotes/{quote_id}/accept"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    let accept_body = response_json(response).await;
    assert_eq!(accept_body["error"]["code"], "wallet_settlement_required");
    assert!(
        accept_body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("wallet-settlement")
    );
}

#[tokio::test]
async fn api_wallet_settlement_serializes_expiry_as_rfc3339_string() {
    let app = test_orchestrator_router().await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/rfq")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(rfq_request().to_string()))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let quote_body = response_json(response).await;
    assert_eq!(quote_body["status"], "accepted");
    let quote_id = quote_body["quote_id"].as_str().expect("quote id");

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/quotes/{quote_id}/wallet-settlement"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "taker_wallet": "DemoTaker111111111111111111111111111111111111",
                        "secret_hash": "0000000000000000000000000000000000000000000000000000000000000000"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let settlement_body = response_json(response).await;
    assert!(settlement_body["expires_at"].is_string());
    assert!(settlement_body["taker_lock_transaction"]["transaction_base64"].is_string());
}

#[tokio::test]
async fn api_rfq_rejects_oversized_request_through_runtime_risk() {
    let app = test_orchestrator_router().await;
    let mut body = rfq_request();
    body["input_amount_raw"] = json!(3_000_000);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/rfq")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let quote_body = response_json(response).await;
    assert_eq!(quote_body["status"], "rejected");
    assert_eq!(quote_body["reason"], "max_notional_exceeded");
    assert_eq!(quote_body["integration_status"], "runtime_orchestrated");
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let app = test_router().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["service"], "firmament");
    assert!(body["version"].is_string());
    assert!(!body["version"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn api_json_error_shape() {
    let app = test_router().await;

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/rfq")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert!(body["error"].is_object());
    assert!(body["error"]["code"].is_string());
    assert!(body["error"]["message"].is_string());
    assert!(body["error"]["details"].is_array());
}

fn usdc() -> AssetId {
    AssetId::from("USDC")
}

fn sol() -> AssetId {
    AssetId::from("SOL")
}

fn cbbtc() -> AssetId {
    AssetId::from("cbBTC")
}

fn quote_inventory() -> BalanceSnapshot {
    BalanceSnapshot {
        wallet: WalletRole::Maker,
        balances: vec![
            TokenAmount::new(usdc(), AmountRaw::new(10_000_000)),
            TokenAmount::new(sol(), AmountRaw::new(200_000_000)),
            TokenAmount::new(cbbtc(), AmountRaw::new(0)),
        ],
        observed_at: OffsetDateTime::now_utc(),
    }
}

fn post_settlement_inventory() -> BalanceSnapshot {
    BalanceSnapshot {
        wallet: WalletRole::Maker,
        balances: vec![
            TokenAmount::new(usdc(), AmountRaw::new(1_000_000)),
            TokenAmount::new(sol(), AmountRaw::new(100_000_000)),
            TokenAmount::new(cbbtc(), AmountRaw::new(0)),
        ],
        observed_at: OffsetDateTime::now_utc(),
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
    async fn reference_price(
        &self,
        pair: AssetPair,
    ) -> Result<ReferencePrice, firmament::AppError> {
        let price = self
            .prices
            .get(&pair)
            .copied()
            .ok_or_else(|| firmament::AppError::unsupported("fake price missing"))?;
        Ok(ReferencePrice {
            pair,
            output_per_input: price,
            observed_at: OffsetDateTime::now_utc(),
        })
    }
}

#[derive(Debug, Clone, Default)]
struct FakeHtlcClient {
    initiated: Arc<Mutex<Vec<HtlcInitiation>>>,
    redeemed: Arc<Mutex<Vec<(firmament::types::TradeId, String)>>>,
    redeemed_legs: Arc<Mutex<HashMap<firmament::types::TradeId, Vec<SettlementLeg>>>>,
}

fn fake_leg_for_funder(funder: WalletRole) -> SettlementLeg {
    match funder {
        WalletRole::Maker => SettlementLeg::MakerOutput,
        WalletRole::Taker | WalletRole::Operator | WalletRole::Gateway => {
            SettlementLeg::TakerInput
        }
    }
}

#[async_trait]
impl HtlcClient for FakeHtlcClient {
    async fn wallet_address(&self, role: WalletRole) -> Result<WalletAddress, firmament::AppError> {
        Ok(WalletAddress::new(match role {
            WalletRole::Maker => "DemoMaker111111111111111111111111111111111111",
            WalletRole::Taker => "DemoTaker111111111111111111111111111111111111",
            WalletRole::Operator => "DemoOperator11111111111111111111111111111111",
            WalletRole::Gateway => "DemoGateway111111111111111111111111111111111",
        }))
    }

    async fn initiate(&self, request: HtlcInitiation) -> Result<HtlcReceipt, firmament::AppError> {
        let mut initiated = self.initiated.lock().expect("htlc lock");
        let leg = fake_leg_for_funder(request.funder);
        let amount = request.amount.clone();
        initiated.push(request.clone());
        Ok(HtlcReceipt {
            trade_id: request.trade_id,
            leg,
            amount,
            status: SettlementStatus::Initiated,
            signature: Some(TxSignature::new(format!("init-{}", initiated.len()))),
        })
    }

    async fn build_external_initiate(
        &self,
        _request: ExternalHtlcInitiation,
    ) -> Result<UnsignedWalletTransaction, firmament::AppError> {
        Ok(UnsignedWalletTransaction {
            transaction_base64: "AA==".to_owned(),
            recent_blockhash: "fake-blockhash".to_owned(),
        })
    }

    async fn redeem(
        &self,
        trade_id: firmament::types::TradeId,
        preimage: String,
    ) -> Result<HtlcReceipt, firmament::AppError> {
        let initiated = self.initiated.lock().expect("htlc lock");
        let trade_initiations: Vec<HtlcInitiation> = initiated
            .iter()
            .filter(|init| init.trade_id == trade_id)
            .cloned()
            .collect();
        drop(initiated);
        if trade_initiations.is_empty() {
            return Err(firmament::AppError::validation(format!(
                "fake htlc client: unknown HTLC trade {trade_id}"
            )));
        }

        // Mirror the production adapter: taker redeem (MakerOutput) runs first,
        // then maker redeem (TakerInput). Pick the first leg that has not yet
        // been redeemed for this trade.
        let mut redeemed_legs = self.redeemed_legs.lock().expect("htlc lock");
        let already = redeemed_legs.entry(trade_id).or_default();
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
                firmament::AppError::validation(format!(
                    "fake htlc client: all legs already redeemed for trade {trade_id}"
                ))
            })?;
        let init = trade_initiations
            .iter()
            .find(|init| fake_leg_for_funder(init.funder) == leg)
            .expect("matching initiation exists");
        let amount = init.amount.clone();
        already.push(leg);
        drop(redeemed_legs);

        let mut redeemed = self.redeemed.lock().expect("htlc lock");
        redeemed.push((trade_id, preimage));
        Ok(HtlcReceipt {
            trade_id,
            leg,
            amount,
            status: SettlementStatus::Redeemed,
            signature: Some(TxSignature::new(format!("redeem-{}", redeemed.len()))),
        })
    }

    async fn refund(
        &self,
        trade_id: firmament::types::TradeId,
    ) -> Result<HtlcReceipt, firmament::AppError> {
        let initiated = self.initiated.lock().expect("htlc lock");
        let init = initiated
            .iter()
            .find(|init| init.trade_id == trade_id)
            .ok_or_else(|| {
                firmament::AppError::validation(format!(
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

    async fn status(
        &self,
        _trade_id: firmament::types::TradeId,
    ) -> Result<SettlementStatus, firmament::AppError> {
        Ok(SettlementStatus::Initiated)
    }
}

#[derive(Debug, Clone, Default)]
struct FakeSwapExecutor;

#[async_trait]
impl SwapExecutor for FakeSwapExecutor {
    async fn quote_swap(&self, request: SwapRequest) -> Result<SwapQuote, firmament::AppError> {
        Ok(SwapQuote {
            expected_output: TokenAmount::new(request.pair.output.clone(), AmountRaw::new(10)),
            estimated_fee: Some(TokenAmount::new(usdc(), AmountRaw::new(10_000))),
            expires_at: None,
            request,
        })
    }

    async fn execute_swap(&self, quote: SwapQuote) -> Result<SwapReceipt, firmament::AppError> {
        Ok(SwapReceipt {
            trade_id: None,
            signature: TxSignature::new("swap-sig"),
            output_amount: Some(quote.expected_output),
        })
    }
}

#[derive(Debug, Clone, Default)]
struct FakeGatewayClient;

#[async_trait]
impl GatewayClient for FakeGatewayClient {
    async fn balance(&self, asset: AssetId) -> Result<GatewayReceipt, firmament::AppError> {
        Ok(GatewayReceipt {
            amount: TokenAmount::new(asset, AmountRaw::new(10_000_000)),
            provider_transfer_id: None,
            signature: None,
        })
    }

    async fn request_refill(
        &self,
        request: GatewayRefillRequest,
    ) -> Result<GatewayReceipt, firmament::AppError> {
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
    async fn balances(&self, _wallet: WalletRole) -> Result<BalanceSnapshot, firmament::AppError> {
        let mut snapshots = self.snapshots.lock().expect("balance lock");
        if snapshots.len() == 1 {
            return Ok(snapshots[0].clone());
        }
        snapshots
            .pop()
            .ok_or_else(|| firmament::AppError::internal("no fake snapshots"))
    }
}
