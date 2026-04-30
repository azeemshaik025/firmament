use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tbd_rfq_maker_runtime::{AppConfig, api, bootstrap};
use tower::ServiceExt;

async fn test_router(token: Option<&str>) -> axum::Router {
    let app_state = bootstrap(AppConfig::default())
        .await
        .expect("bootstrap state");
    api::router_with_operator_token(&app_state, token.map(str::to_owned))
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
    let app = test_router(Some("secret-token")).await;

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
    assert_eq!(body["state"]["inventory"]["status"], "scaffold_ready");
}

#[tokio::test]
async fn api_runtime_events_endpoint_success() {
    let app = test_router(Some("secret-token")).await;

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
async fn api_mutating_endpoint_rejects_missing_token() {
    let app = test_router(Some("secret-token")).await;

    let response = app
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

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "operator_token_missing");
}

#[tokio::test]
async fn api_mutating_endpoint_rejects_invalid_token() {
    let app = test_router(Some("secret-token")).await;

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/rfq")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, "Bearer wrong-token")
                .body(Body::from(rfq_request().to_string()))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "operator_token_invalid");
}

#[tokio::test]
async fn api_mutating_endpoint_accepts_valid_token_and_reaches_placeholder_service() {
    let app = test_router(Some("secret-token")).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/rfq")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, "Bearer secret-token")
                .body(Body::from(rfq_request().to_string()))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let quote_body = response_json(response).await;
    assert_eq!(quote_body["status"], "accepted");
    assert_eq!(quote_body["integration_status"], "placeholder");
    let quote_id = quote_body["quote_id"].as_str().expect("quote id");

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/quotes/{quote_id}/accept"))
                .header(header::AUTHORIZATION, "Bearer secret-token")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accept_body = response_json(response).await;
    assert_eq!(accept_body["quote_id"], quote_id);
    assert_eq!(accept_body["settlement_status"], "pending");
    assert_eq!(accept_body["integration_status"], "placeholder");
}

#[tokio::test]
async fn api_json_error_shape() {
    let app = test_router(Some("secret-token")).await;

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/rfq")
                .header(header::AUTHORIZATION, "Bearer wrong-token")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert!(body["error"].is_object());
    assert!(body["error"]["code"].is_string());
    assert!(body["error"]["message"].is_string());
    assert!(body["error"]["details"].is_array());
}
