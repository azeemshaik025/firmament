use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use firmament::{AppConfig, api, bootstrap};
use tower::ServiceExt;

async fn test_router() -> axum::Router {
    let app_state = bootstrap(AppConfig::default())
        .await
        .expect("bootstrap state");
    api::router(&app_state)
}

#[tokio::test]
async fn api_responses_include_generated_request_id() {
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
    let request_id = response
        .headers()
        .get("x-request-id")
        .expect("x-request-id header")
        .to_str()
        .expect("request id header is ascii");
    uuid::Uuid::parse_str(request_id).expect("generated request id is a UUID");
}

#[tokio::test]
async fn api_preserves_inbound_request_id() {
    let app = test_router().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header(
                    header::HeaderName::from_static("x-request-id"),
                    "demo-request-123",
                )
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .expect("x-request-id header"),
        "demo-request-123"
    );
}
