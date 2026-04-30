use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tbd_rfq_maker_runtime::ports::{
    BalanceReader, GatewayClient, HtlcClient, PriceProvider, SwapExecutor,
};
use tbd_rfq_maker_runtime::runtime::{
    RuntimeAdapters, RuntimeOrchestrator, RuntimeOrchestratorOptions, RuntimePersistence,
};
use tbd_rfq_maker_runtime::types::{
    AmountRaw, AssetId, AssetPair, BalanceSnapshot, GatewayReceipt, GatewayRefillRequest,
    HtlcInitiation, HtlcReceipt, ReferencePrice, SettlementStatus, SwapQuote, SwapReceipt,
    SwapRequest, TokenAmount, TxSignature, WalletRole,
};
use tbd_rfq_maker_runtime::{AppConfig, api, bootstrap};
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
        Arc::new(
            RuntimePersistence::open_in_memory(
                tbd_rfq_maker_runtime::assets::AssetRegistry::default(),
            )
            .expect("persistence"),
        ),
        RuntimeOrchestratorOptions::default(),
    );
    api::router_with_orchestrator(Arc::new(orchestrator))
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
async fn api_rfq_accept_flow_uses_runtime_orchestrator_and_fake_adapters() {
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

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accept_body = response_json(response).await;
    assert_eq!(accept_body["quote_id"], quote_id);
    assert_eq!(accept_body["settlement_status"], "redeemed");
    assert_eq!(accept_body["integration_status"], "runtime_orchestrated");
    assert!(accept_body["tx_signatures"].as_array().unwrap().len() >= 4);
    assert!(
        accept_body["ledger_summary"]["entry_count"]
            .as_u64()
            .unwrap()
            > 0
    );
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
    ) -> Result<ReferencePrice, tbd_rfq_maker_runtime::AppError> {
        let price =
            self.prices.get(&pair).copied().ok_or_else(|| {
                tbd_rfq_maker_runtime::AppError::unsupported("fake price missing")
            })?;
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
    redeemed: Arc<Mutex<Vec<(tbd_rfq_maker_runtime::types::TradeId, String)>>>,
}

#[async_trait]
impl HtlcClient for FakeHtlcClient {
    async fn initiate(
        &self,
        request: HtlcInitiation,
    ) -> Result<HtlcReceipt, tbd_rfq_maker_runtime::AppError> {
        let mut initiated = self.initiated.lock().expect("htlc lock");
        initiated.push(request.clone());
        Ok(HtlcReceipt {
            trade_id: request.trade_id,
            status: SettlementStatus::Initiated,
            signature: Some(TxSignature::new(format!("init-{}", initiated.len()))),
        })
    }

    async fn redeem(
        &self,
        trade_id: tbd_rfq_maker_runtime::types::TradeId,
        preimage: String,
    ) -> Result<HtlcReceipt, tbd_rfq_maker_runtime::AppError> {
        let mut redeemed = self.redeemed.lock().expect("htlc lock");
        redeemed.push((trade_id, preimage));
        Ok(HtlcReceipt {
            trade_id,
            status: SettlementStatus::Redeemed,
            signature: Some(TxSignature::new(format!("redeem-{}", redeemed.len()))),
        })
    }

    async fn refund(
        &self,
        trade_id: tbd_rfq_maker_runtime::types::TradeId,
    ) -> Result<HtlcReceipt, tbd_rfq_maker_runtime::AppError> {
        Ok(HtlcReceipt {
            trade_id,
            status: SettlementStatus::Refunded,
            signature: Some(TxSignature::new("refund")),
        })
    }

    async fn status(
        &self,
        _trade_id: tbd_rfq_maker_runtime::types::TradeId,
    ) -> Result<SettlementStatus, tbd_rfq_maker_runtime::AppError> {
        Ok(SettlementStatus::Initiated)
    }
}

#[derive(Debug, Clone, Default)]
struct FakeSwapExecutor;

#[async_trait]
impl SwapExecutor for FakeSwapExecutor {
    async fn quote_swap(
        &self,
        request: SwapRequest,
    ) -> Result<SwapQuote, tbd_rfq_maker_runtime::AppError> {
        Ok(SwapQuote {
            expected_output: TokenAmount::new(request.pair.output.clone(), AmountRaw::new(10)),
            estimated_fee: Some(TokenAmount::new(usdc(), AmountRaw::new(10_000))),
            expires_at: None,
            request,
        })
    }

    async fn execute_swap(
        &self,
        quote: SwapQuote,
    ) -> Result<SwapReceipt, tbd_rfq_maker_runtime::AppError> {
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
    async fn balance(
        &self,
        asset: AssetId,
    ) -> Result<GatewayReceipt, tbd_rfq_maker_runtime::AppError> {
        Ok(GatewayReceipt {
            amount: TokenAmount::new(asset, AmountRaw::new(10_000_000)),
            provider_transfer_id: None,
            signature: None,
        })
    }

    async fn request_refill(
        &self,
        request: GatewayRefillRequest,
    ) -> Result<GatewayReceipt, tbd_rfq_maker_runtime::AppError> {
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
    async fn balances(
        &self,
        _wallet: WalletRole,
    ) -> Result<BalanceSnapshot, tbd_rfq_maker_runtime::AppError> {
        let mut snapshots = self.snapshots.lock().expect("balance lock");
        if snapshots.len() == 1 {
            return Ok(snapshots[0].clone());
        }
        snapshots
            .pop()
            .ok_or_else(|| tbd_rfq_maker_runtime::AppError::internal("no fake snapshots"))
    }
}
