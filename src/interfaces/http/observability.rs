//! HTTP observability middleware.

use std::time::Instant;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use axum::middleware::Next;
use tracing::{Instrument, debug, error, info, info_span, warn};
use uuid::Uuid;

const REQUEST_ID_HEADER: &str = "x-request-id";

pub(crate) async fn log_request(mut request: Request<Body>, next: Next) -> Response<Body> {
    let request_id = request_id(request.headers()).unwrap_or_else(generate_request_id);
    request
        .headers_mut()
        .insert(REQUEST_ID_HEADER, request_id.clone());

    let request_id_text = request_id
        .to_str()
        .map_or_else(|_| "invalid-request-id".to_owned(), str::to_owned);
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let is_health_check = path == "/health";
    let started_at = Instant::now();

    let span = info_span!(
        "http_request",
        request_id = %request_id_text,
        method = %method,
        path = %path
    );

    async move {
        if is_health_check {
            debug!("HTTP request started");
        } else {
            info!("HTTP request started");
        }

        let mut response = next.run(request).await;
        let status = response.status();
        response
            .headers_mut()
            .insert(REQUEST_ID_HEADER, request_id.clone());
        log_response(status, started_at, is_health_check);
        response
    }
    .instrument(span)
    .await
}

fn request_id(headers: &HeaderMap) -> Option<HeaderValue> {
    let value = headers.get(REQUEST_ID_HEADER)?;
    let text = value.to_str().ok()?.trim();
    (!text.is_empty()).then(|| value.clone())
}

fn generate_request_id() -> HeaderValue {
    HeaderValue::from_str(&Uuid::now_v7().to_string())
        .expect("UUID request ids are valid HTTP header values")
}

fn log_response(status: StatusCode, started_at: Instant, is_health_check: bool) {
    let latency_ms = started_at.elapsed().as_secs_f64() * 1000.0;
    if status.is_server_error() {
        error!(status = status.as_u16(), latency_ms, "HTTP request failed");
    } else if status.is_client_error() {
        warn!(
            status = status.as_u16(),
            latency_ms, "HTTP request rejected"
        );
    } else if is_health_check {
        debug!(
            status = status.as_u16(),
            latency_ms, "HTTP request completed"
        );
    } else {
        info!(
            status = status.as_u16(),
            latency_ms, "HTTP request completed"
        );
    }
}
