//! Local Axum operator API for RFQ route shells and runtime projection reads.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use time::{Duration, OffsetDateTime};
use tokio::net::TcpListener;
use tracing::info;

use crate::api_types::{
    ErrorBody, ErrorResponse, HtlcAcceptanceTerms, IntegrationStatus, LedgerSummary,
    QuoteAcceptResponse, RfqRequest, RfqResponse, RuntimeEventsResponse, RuntimeStateResponse,
    TradeAmounts, TradeResponse,
};
use crate::config::AppConfig;
use crate::error::{AppError, AppResult};
use crate::events::{EventMetadata, QuoteEvent, RuntimeEvent, SettlementEvent};
use crate::runtime::{AppState, RuntimeHandle};
use crate::types::{
    AssetId, AssetPair, MintAddress, QuoteId, SettlementStatus, TokenAmount, TradeId,
};

const OPERATOR_TOKEN_HEADER: &str = "x-operator-api-token";

/// Service boundary used by the API while quote, settlement, and ledger workers
/// are integrated in later modules.
#[async_trait]
pub trait RfqApiService: Send + Sync {
    /// Handle an RFQ request.
    async fn request_quote(&self, request: RfqRequest) -> Result<RfqResponse, ApiError>;

    /// Accept an existing quote and create a trade shell.
    async fn accept_quote(&self, quote_id: QuoteId) -> Result<QuoteAcceptResponse, ApiError>;

    /// Read the current trade shell.
    async fn trade(&self, trade_id: TradeId) -> Result<TradeResponse, ApiError>;
}

/// Build the production router, reading the operator token from the configured
/// environment variable.
pub fn router(app_state: &AppState) -> Router {
    let operator_token = operator_token_from_env(app_state.config());
    router_with_operator_token(app_state, operator_token)
}

/// Build a router with an explicit operator token. This is primarily used by
/// tests and local harnesses.
pub fn router_with_operator_token(app_state: &AppState, operator_token: Option<String>) -> Router {
    let service = Arc::new(PlaceholderRfqApiService::new(app_state));
    router_with_service(app_state.runtime(), operator_token, service)
}

/// Build a router with an injected RFQ service implementation.
pub fn router_with_service(
    runtime: RuntimeHandle,
    operator_token: Option<String>,
    service: Arc<dyn RfqApiService>,
) -> Router {
    let context = Arc::new(ApiContext {
        runtime,
        service,
        operator_token: normalize_operator_token(operator_token),
    });

    Router::new()
        .route("/v1/rfq", post(post_rfq))
        .route("/v1/quotes/{quote_id}/accept", post(post_quote_accept))
        .route("/v1/trades/{trade_id}", get(get_trade))
        .route("/v1/runtime/state", get(get_runtime_state))
        .route("/v1/runtime/events", get(get_runtime_events))
        .with_state(context)
}

/// Return true when the current config should start the local HTTP API.
#[must_use]
pub const fn enabled(config: &AppConfig) -> bool {
    config.http.port != 0
}

/// Serve the local HTTP API until the process is stopped.
///
/// # Errors
///
/// Returns an error when the configured address is invalid, the socket cannot
/// bind, or the HTTP server exits with an error.
pub async fn serve(app_state: AppState) -> AppResult<()> {
    let address = bind_address(app_state.config())?;
    let router = router(&app_state);
    let listener = TcpListener::bind(address)
        .await
        .map_err(|error| AppError::internal(format!("bind HTTP API on {address}: {error}")))?;

    info!(%address, "local operator API listening");
    axum::serve(listener, router)
        .await
        .map_err(|error| AppError::internal(format!("serve HTTP API: {error}")))
}

#[derive(Clone)]
struct ApiContext {
    runtime: RuntimeHandle,
    service: Arc<dyn RfqApiService>,
    operator_token: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
struct PlaceholderRfqApiService {
    runtime: RuntimeHandle,
    config: AppConfig,
}

impl PlaceholderRfqApiService {
    fn new(app_state: &AppState) -> Self {
        Self {
            runtime: app_state.runtime(),
            config: app_state.config().clone(),
        }
    }

    fn asset_id_for_mint(&self, mint: &MintAddress) -> AssetId {
        self.config
            .assets
            .supported
            .iter()
            .find(|asset| asset.mint == *mint)
            .map_or_else(|| AssetId::new(mint.as_str()), |asset| asset.id.clone())
    }
}

#[async_trait]
impl RfqApiService for PlaceholderRfqApiService {
    async fn request_quote(&self, request: RfqRequest) -> Result<RfqResponse, ApiError> {
        let quote_id = QuoteId::generate();
        let input_asset = self.asset_id_for_mint(&request.input_mint);
        let output_asset = self.asset_id_for_mint(&request.output_mint);
        let expires_at = expiry_timestamp(
            request
                .expiry_seconds
                .unwrap_or(self.config.assets.policy.default_quote_expiry_seconds),
        )?;

        self.runtime
            .publish_event(RuntimeEvent::Quote(QuoteEvent::Requested {
                metadata: EventMetadata::new(self.runtime.snapshot().await.run_id),
                quote_id,
                pair: AssetPair::new(input_asset.clone(), output_asset),
                input_amount: TokenAmount::new(input_asset, request.input_amount_raw),
                taker_wallet: request.taker_wallet,
                expires_at,
            }))
            .await
            .map_err(ApiError::from_app_error)?;

        Ok(RfqResponse::Accepted {
            quote_id,
            quoted_output_amount_raw: crate::types::AmountRaw::new(0),
            spread_bps: self.config.assets.policy.base_spread_bps,
            expires_at,
            htlc_terms: HtlcAcceptanceTerms {
                settlement_model: "solana_htlc".to_owned(),
                escrow_mint: request.output_mint,
                expires_at,
                hashlock: None,
                placeholder: true,
            },
            risk_checks: vec!["placeholder_service_reached".to_owned()],
            integration_status: IntegrationStatus::Placeholder,
        })
    }

    async fn accept_quote(&self, quote_id: QuoteId) -> Result<QuoteAcceptResponse, ApiError> {
        let trade_id = TradeId::generate();
        let snapshot = self.runtime.snapshot().await;

        self.runtime
            .publish_event(RuntimeEvent::Quote(QuoteEvent::Accepted {
                metadata: EventMetadata::new(snapshot.run_id),
                quote_id,
                trade_id,
            }))
            .await
            .map_err(ApiError::from_app_error)?;

        self.runtime
            .publish_event(RuntimeEvent::Settlement(SettlementEvent::Started {
                metadata: EventMetadata::new(snapshot.run_id),
                trade_id,
                quote_id,
            }))
            .await
            .map_err(ApiError::from_app_error)?;

        Ok(QuoteAcceptResponse {
            quote_id,
            trade_id,
            settlement_status: SettlementStatus::Pending,
            tx_signatures: Vec::new(),
            ledger_summary: LedgerSummary::default(),
            integration_status: IntegrationStatus::Placeholder,
        })
    }

    async fn trade(&self, trade_id: TradeId) -> Result<TradeResponse, ApiError> {
        Ok(TradeResponse {
            trade_id,
            settlement_status: SettlementStatus::Pending,
            tx_signatures: Vec::new(),
            amounts: TradeAmounts::default(),
            ledger_summary: LedgerSummary::default(),
            integration_status: IntegrationStatus::Placeholder,
        })
    }
}

async fn post_rfq(
    State(context): State<Arc<ApiContext>>,
    headers: HeaderMap,
    payload: Result<Json<RfqRequest>, JsonRejection>,
) -> Result<Response<Body>, ApiError> {
    require_operator_token(&headers, &context)?;
    let Json(request) = payload.map_err(|error| ApiError::from_json_rejection(&error))?;
    let response = context.service.request_quote(request).await?;
    Ok(Json(response).into_response())
}

async fn post_quote_accept(
    State(context): State<Arc<ApiContext>>,
    headers: HeaderMap,
    path: Result<Path<QuoteId>, PathRejection>,
) -> Result<Response<Body>, ApiError> {
    require_operator_token(&headers, &context)?;
    let Path(quote_id) = path.map_err(|error| ApiError::from_path_rejection(&error))?;
    let response = context.service.accept_quote(quote_id).await?;
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

async fn get_trade(
    State(context): State<Arc<ApiContext>>,
    path: Result<Path<TradeId>, PathRejection>,
) -> Result<Response<Body>, ApiError> {
    let Path(trade_id) = path.map_err(|error| ApiError::from_path_rejection(&error))?;
    let response = context.service.trade(trade_id).await?;
    Ok(Json(response).into_response())
}

async fn get_runtime_state(
    State(context): State<Arc<ApiContext>>,
) -> Result<Json<RuntimeStateResponse>, ApiError> {
    Ok(Json(RuntimeStateResponse {
        state: context.runtime.snapshot().await,
    }))
}

async fn get_runtime_events(
    State(context): State<Arc<ApiContext>>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<RuntimeEventsResponse>, ApiError> {
    let events = context.runtime.recent_events(query.limit).await;
    Ok(Json(RuntimeEventsResponse {
        count: events.len(),
        events,
    }))
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct EventsQuery {
    limit: Option<usize>,
}

/// Error type converted into the stable JSON API envelope.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Vec<String>,
}

impl ApiError {
    fn new(
        status: StatusCode,
        code: &'static str,
        message: impl Into<String>,
        details: Vec<String>,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details,
        }
    }

    fn operator_token_missing() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "operator_token_missing",
            "operator API token is required",
            Vec::new(),
        )
    }

    fn operator_token_invalid() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "operator_token_invalid",
            "operator API token is invalid",
            Vec::new(),
        )
    }

    fn operator_token_unconfigured() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "operator_token_unconfigured",
            "operator API token is not configured",
            Vec::new(),
        )
    }

    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message, Vec::new())
    }

    fn from_json_rejection(error: &JsonRejection) -> Self {
        Self::bad_request("invalid_json", error.body_text())
    }

    fn from_path_rejection(error: &PathRejection) -> Self {
        Self::bad_request("invalid_path", error.body_text())
    }

    fn from_app_error(error: AppError) -> Self {
        match error {
            AppError::Validation(message) => Self::bad_request("validation_failed", message),
            AppError::Unsupported(message) => Self::new(
                StatusCode::NOT_IMPLEMENTED,
                "unsupported_operation",
                message,
                Vec::new(),
            ),
            AppError::ExternalService { service, message } => Self::new(
                StatusCode::BAD_GATEWAY,
                "external_service_unavailable",
                format!("external service {service} is unavailable"),
                vec![message],
            ),
            AppError::Config(_)
            | AppError::Solana(_)
            | AppError::Persistence(_)
            | AppError::Internal(_) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "internal API error",
                Vec::new(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorResponse {
            error: ErrorBody {
                code: self.code.to_owned(),
                message: self.message,
                details: self.details,
            },
        };

        (self.status, Json(body)).into_response()
    }
}

fn require_operator_token(headers: &HeaderMap, context: &ApiContext) -> Result<(), ApiError> {
    let Some(expected_token) = context.operator_token.as_deref() else {
        return Err(ApiError::operator_token_unconfigured());
    };

    let Some(provided_token) = provided_operator_token(headers)? else {
        return Err(ApiError::operator_token_missing());
    };

    if provided_token == expected_token {
        Ok(())
    } else {
        Err(ApiError::operator_token_invalid())
    }
}

fn provided_operator_token(headers: &HeaderMap) -> Result<Option<&str>, ApiError> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let value = value
            .to_str()
            .map_err(|_| ApiError::operator_token_invalid())?;
        let Some(token) = value.strip_prefix("Bearer ") else {
            return Err(ApiError::operator_token_invalid());
        };
        let token = token.trim();
        if token.is_empty() {
            return Err(ApiError::operator_token_invalid());
        }
        return Ok(Some(token));
    }

    if let Some(value) = headers.get(OPERATOR_TOKEN_HEADER) {
        let token = value
            .to_str()
            .map_err(|_| ApiError::operator_token_invalid())?
            .trim();
        if token.is_empty() {
            return Err(ApiError::operator_token_invalid());
        }
        return Ok(Some(token));
    }

    Ok(None)
}

fn normalize_operator_token(operator_token: Option<String>) -> Option<Arc<str>> {
    operator_token.and_then(|token| {
        let token = token.trim();
        (!token.is_empty()).then(|| Arc::<str>::from(token.to_owned()))
    })
}

fn operator_token_from_env(config: &AppConfig) -> Option<String> {
    std::env::var(&config.http.operator_api_token_env)
        .ok()
        .filter(|token| !token.trim().is_empty())
}

fn expiry_timestamp(expiry_seconds: u64) -> Result<OffsetDateTime, ApiError> {
    let seconds = i64::try_from(expiry_seconds)
        .map_err(|_| ApiError::bad_request("invalid_expiry", "expiry_seconds is too large"))?;
    Ok(OffsetDateTime::now_utc() + Duration::seconds(seconds))
}

fn bind_address(config: &AppConfig) -> AppResult<SocketAddr> {
    format!("{}:{}", config.http.bind_address, config.http.port)
        .parse()
        .map_err(|error| AppError::validation(format!("invalid HTTP bind address: {error}")))
}
