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
async fn ledger_endpoint_returns_balances() {
    let app = test_orchestrator_router().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/ledger")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["healthy"], true);
    assert!(body["entry_count"].as_u64().unwrap() > 0);
    let balances = body["balances"].as_array().expect("balances array");
    assert!(balances.iter().any(|b| {
        b["account_type"] == "working_custody"
            && b["asset"] == "USDC"
            && b["balance_raw"].is_string()
            && b["decimals"].as_u64() == Some(6)
            && b["display_amount"].is_string()
    }));
}

#[tokio::test]
async fn ledger_endpoint_filters_by_account_type() {
    let app = test_orchestrator_router().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/ledger?account_type=working_custody")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let balances = body["balances"].as_array().expect("balances array");
    assert!(!balances.is_empty(), "expected at least one filtered entry");
    for entry in balances {
        assert_eq!(entry["account_type"], "working_custody");
    }
    // entry_count is global, so it includes the unfiltered total.
    assert!(body["entry_count"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn ledger_endpoint_rejects_unknown_account_type() {
    let app = test_orchestrator_router().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/ledger?account_type=foo")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "invalid_account_type");
    assert!(body["error"]["message"].as_str().unwrap().contains("foo"));
}

/// Build a router and return its underlying persistence handle so privacy
/// tests can hand-craft ledger entries directly. Mirrors
/// `test_orchestrator_router` but exposes the persistence Arc.
async fn test_orchestrator_with_persistence() -> (axum::Router, Arc<RuntimePersistence>) {
    let app_state = bootstrap(AppConfig::default())
        .await
        .expect("bootstrap state");
    let persistence = Arc::new(
        RuntimePersistence::open_in_memory(firmament::assets::AssetRegistry::default())
            .expect("persistence"),
    );
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
        persistence.clone(),
        RuntimeOrchestratorOptions::default(),
    );
    let router = api::router_with_orchestrator(Arc::new(orchestrator));
    (router, persistence)
}

#[tokio::test]
async fn ledger_endpoint_omits_wallet_addresses() {
    let app = test_orchestrator_router().await;
    let _ = drive_completed_trade(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/ledger")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;

    let banned_wallets = [
        "DemoTaker111111111111111111111111111111111111",
        "DemoMaker111111111111111111111111111111111111",
        "DemoOperator11111111111111111111111111111111",
        "DemoGateway111111111111111111111111111111111",
    ];

    walk_json_strings(&body, &mut |value| {
        for banned in &banned_wallets {
            assert!(
                !value.contains(banned),
                "ledger response leaked wallet address {banned}: {value}"
            );
        }
        // Solana base58-encoded 32-byte pubkeys are typically 43 or 44
        // characters, all base58 alphabet, no dashes. UUIDs (36 chars, with
        // dashes) and signatures (86-88 chars) are explicitly allowed.
        // Reject any pubkey-shaped string that does not appear in the
        // documented allow-list (asset IDs, account-type tags, etc).
        assert!(
            !((value.len() == 43 || value.len() == 44) && is_base58_alphabet(value)),
            "ledger response leaked Solana-pubkey-shaped string: {value}"
        );
    });
}

fn is_base58_alphabet(value: &str) -> bool {
    const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    !value.is_empty() && value.bytes().all(|byte| BASE58.contains(&byte))
}

fn walk_json_strings(value: &Value, visitor: &mut impl FnMut(&str)) {
    match value {
        Value::String(s) => visitor(s),
        Value::Array(items) => {
            for item in items {
                walk_json_strings(item, visitor);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                visitor(key);
                walk_json_strings(item, visitor);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn ledger_endpoint_marks_unhealthy_on_negative_protected_account() {
    let (app, persistence) = test_orchestrator_with_persistence().await;

    // Hand-craft a balanced ledger transaction that drives `working_custody`
    // negative for an asset it never held: credit the working_custody account
    // and debit a counter-account so the transaction balances per-asset, but
    // leaves `working_custody:cbBTC` at -100. cbBTC is a protected account
    // type (working custody) so the public read endpoint must report
    // `healthy=false`.
    let asset = AssetId::from("cbBTC");
    let amount = AmountRaw::new(100);
    let transaction = firmament::ledger::LedgerTransactionBuilder::new(
        "test_negative_protected",
        uuid::Uuid::now_v7(),
    )
    .description("force a protected-account negative balance for healthy-flag test")
    .idempotency_key(format!(
        "test:api:negative_protected:{}:{}",
        asset.as_str(),
        uuid::Uuid::now_v7(),
    ))
    .credit(
        firmament::ledger::LedgerAccountId::working(asset.clone()),
        amount,
    )
    .debit(
        firmament::ledger::LedgerAccountId::external(asset.clone(), "drift_fixture"),
        amount,
    )
    .build()
    .expect("balanced fixture transaction");
    persistence
        .save_ledger_transaction(&transaction)
        .expect("persist fixture transaction");

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/ledger")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(
        body["healthy"], false,
        "expected healthy=false when working_custody:cbBTC is negative"
    );
    let working = body["balances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["account_type"] == "working_custody" && b["asset"] == "cbBTC")
        .expect("working_custody:cbBTC entry present");
    assert_eq!(working["balance_raw"].as_str(), Some("-100"));

    // Rebuild the orchestrator on a fresh persistence so this fixture cannot
    // pollute other tests in this file. Rust's tokio::test isolation means
    // each test gets its own `app` and `persistence` already, but be explicit
    // about cleanup intent: drop the persistence handle.
    drop(persistence);
}

async fn drive_completed_trade(app: axum::Router) -> serde_json::Value {
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
    let quote_body = response_json(response).await;
    let quote_id = quote_body["quote_id"]
        .as_str()
        .expect("quote id")
        .to_owned();

    let response = app
        .clone()
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
    let settlement_body = response_json(response).await;
    let trade_id = settlement_body["trade_id"]
        .as_str()
        .expect("trade id")
        .to_owned();

    let lock_signature = "5".repeat(88);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/trades/{trade_id}/taker-lock"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "signature": lock_signature }).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    let _ = response_json(response).await;

    let redeem_signature = "6".repeat(88);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/trades/{trade_id}/taker-redeem"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "preimage": "00".repeat(32),
                        "signature": redeem_signature
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    response_json(response).await
}

#[tokio::test]
async fn trades_endpoint_returns_recent_trades() {
    let app = test_orchestrator_router().await;
    let _ = drive_completed_trade(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/trades")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert!(body["total_count"].as_u64().unwrap() >= 1);
    assert!(body["successful_count"].as_u64().unwrap() >= 1);
    let trades = body["trades"].as_array().expect("trades array");
    assert_eq!(trades.len(), 1);
    assert!(trades[0]["trade_id"].is_string());
    assert!(trades[0]["quote_id"].is_string());
    assert_eq!(trades[0]["settlement_status"], "redeemed");
    assert_eq!(trades[0]["input"]["asset"], "USDC");
    assert!(trades[0]["input"]["amount_raw"].is_string());
    assert_eq!(trades[0]["input"]["decimals"].as_u64(), Some(6));
    assert_eq!(trades[0]["output"]["asset"], "SOL");
    assert!(trades[0]["output"]["amount_raw"].is_string());
    assert_eq!(trades[0]["output"]["decimals"].as_u64(), Some(9));
}

#[tokio::test]
async fn trades_endpoint_respects_limit_param() {
    let app = test_orchestrator_router().await;
    let _ = drive_completed_trade(app.clone()).await;
    let _ = drive_completed_trade(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/trades?limit=1")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["trades"].as_array().unwrap().len(), 1);
    assert!(body["total_count"].as_u64().unwrap() >= 2);
}

#[tokio::test]
async fn trades_endpoint_caps_at_100() {
    let app = test_orchestrator_router().await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/trades?limit=500")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "invalid_limit");

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/trades?limit=0")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "invalid_limit");
}

#[tokio::test]
async fn trades_endpoint_includes_tx_signatures_with_kinds() {
    let app = test_orchestrator_router().await;
    let _ = drive_completed_trade(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/trades")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    let body = response_json(response).await;
    let signatures = body["trades"][0]["tx_signatures"]
        .as_array()
        .expect("tx_signatures");
    assert!(!signatures.is_empty(), "expected at least one tx signature");
    let kinds: Vec<&str> = signatures
        .iter()
        .map(|s| s["kind"].as_str().expect("kind str"))
        .collect();
    let allowed = [
        "taker_lock",
        "taker_redeem",
        "taker_refund",
        "maker_lock",
        "maker_redeem",
        "maker_refund",
        "gateway_burn",
        "gateway_mint",
        "jupiter_swap",
    ];
    for kind in &kinds {
        assert!(
            allowed.contains(kind),
            "unexpected tx_signatures.kind '{kind}'"
        );
    }
    assert!(kinds.contains(&"taker_lock"), "expected taker_lock kind");
    assert!(kinds.contains(&"maker_lock"), "expected maker_lock kind");
    assert!(
        kinds.contains(&"taker_redeem"),
        "expected taker_redeem kind"
    );
    assert!(
        kinds.contains(&"maker_redeem"),
        "expected maker_redeem kind"
    );
    for sig in signatures {
        assert!(!sig["signature"].as_str().unwrap().is_empty());
    }
}

#[tokio::test]
async fn trades_endpoint_excludes_rebalance_signatures() {
    // Background rebalance swaps fire from the post-settlement automation pass
    // and are not bound to a trade. Verify trade-summary tx_signatures contains
    // no `jupiter_swap` entries when there is no Gateway-to-DEX execution path.
    let app = test_orchestrator_router().await;
    let _ = drive_completed_trade(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/runtime/trades")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let body = response_json(response).await;
    let signatures = body["trades"][0]["tx_signatures"]
        .as_array()
        .expect("tx_signatures");
    assert!(
        signatures
            .iter()
            .all(|s| s["kind"].as_str() != Some("jupiter_swap")),
        "rebalance jupiter_swap signature must not appear on a non-Gateway trade"
    );
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
        WalletRole::Taker | WalletRole::Operator | WalletRole::Gateway => SettlementLeg::TakerInput,
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

    async fn record_external_initiate(
        &self,
        request: ExternalHtlcInitiation,
        signature: TxSignature,
    ) -> Result<HtlcReceipt, firmament::AppError> {
        let role_request = HtlcInitiation {
            trade_id: request.trade_id,
            funder: WalletRole::Taker,
            redeemer: WalletRole::Maker,
            amount: request.amount.clone(),
            hashlock: request.hashlock,
            expires_at: request.expires_at,
        };
        let mut initiated = self.initiated.lock().expect("htlc lock");
        initiated.push(role_request);
        drop(initiated);
        Ok(HtlcReceipt {
            trade_id: request.trade_id,
            leg: SettlementLeg::TakerInput,
            amount: request.amount,
            status: SettlementStatus::Initiated,
            signature: Some(signature),
        })
    }

    async fn initiate_with_external_redeemer(
        &self,
        request: HtlcInitiation,
        _redeemer: WalletAddress,
    ) -> Result<HtlcReceipt, firmament::AppError> {
        let mut initiated = self.initiated.lock().expect("htlc lock");
        let leg = fake_leg_for_funder(request.funder);
        let amount = request.amount.clone();
        initiated.push(request.clone());
        Ok(HtlcReceipt {
            trade_id: request.trade_id,
            leg,
            amount,
            status: SettlementStatus::Initiated,
            signature: Some(TxSignature::new(format!("init-ext-{}", initiated.len()))),
        })
    }

    async fn build_external_redeem(
        &self,
        _trade_id: firmament::types::TradeId,
        _redeemer: WalletAddress,
        _preimage: String,
    ) -> Result<UnsignedWalletTransaction, firmament::AppError> {
        Ok(UnsignedWalletTransaction {
            transaction_base64: "AA==".to_owned(),
            recent_blockhash: "fake-blockhash".to_owned(),
        })
    }

    async fn record_external_redeem(
        &self,
        trade_id: firmament::types::TradeId,
        signature: TxSignature,
    ) -> Result<HtlcReceipt, firmament::AppError> {
        // Mirror real behaviour: this path records the taker's redeem on the
        // maker-output leg (the maker's HTLC was redeemed BY the taker).
        let initiated = self.initiated.lock().expect("htlc lock");
        let init = initiated
            .iter()
            .find(|init| init.trade_id == trade_id && init.funder == WalletRole::Maker)
            .ok_or_else(|| {
                firmament::AppError::validation(format!(
                    "fake htlc client: no maker-funded leg for trade {trade_id}"
                ))
            })?;
        let amount = init.amount.clone();
        drop(initiated);
        let mut redeemed_legs = self.redeemed_legs.lock().expect("htlc lock");
        redeemed_legs
            .entry(trade_id)
            .or_default()
            .push(SettlementLeg::MakerOutput);
        Ok(HtlcReceipt {
            trade_id,
            leg: SettlementLeg::MakerOutput,
            amount,
            status: SettlementStatus::Redeemed,
            signature: Some(signature),
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
