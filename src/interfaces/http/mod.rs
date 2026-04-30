//! Local Axum operator API for RFQ route shells and runtime projection reads.

pub mod types;

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::net::TcpListener;
use tracing::info;

use crate::application::rfq;
use crate::application::runtime::{AppState, RuntimeHandle, RuntimeOrchestrator, RuntimeTrade};
use crate::config::AppConfig;
use crate::domain::types::{MintAddress, QuoteId, TradeId};
use crate::error::{AppError, AppResult};
use crate::interfaces::http::types::{
    ErrorBody, ErrorResponse, HtlcAcceptanceTerms, IntegrationStatus, LedgerSummary,
    QuoteAcceptResponse, RfqRequest, RfqResponse, RuntimeEventsResponse, RuntimeStateResponse,
    TradeAmounts, TradeResponse,
};

/// Service boundary used by the API for quote, settlement, and trade reads.
#[async_trait]
pub trait RfqApiService: Send + Sync {
    /// Handle an RFQ request.
    async fn request_quote(&self, request: RfqRequest) -> Result<RfqResponse, ApiError>;

    /// Accept an existing quote and create a trade shell.
    async fn accept_quote(&self, quote_id: QuoteId) -> Result<QuoteAcceptResponse, ApiError>;

    /// Read the current trade shell.
    async fn trade(&self, trade_id: TradeId) -> Result<TradeResponse, ApiError>;
}

/// Build a read-only router for a bootstrapped runtime projection. Mutating RFQ
/// routes return a clear unavailable error unless an orchestrator-backed
/// service is injected.
pub fn router(app_state: &AppState) -> Router {
    let service = Arc::new(DisabledRfqApiService::new(
        "RFQ orchestrator is not attached to this API router",
    ));
    router_with_service(app_state.runtime(), service)
}

/// Build a production router backed by the runtime orchestrator.
pub fn router_with_orchestrator(orchestrator: Arc<RuntimeOrchestrator>) -> Router {
    let runtime = orchestrator.runtime();
    let service = Arc::new(OrchestratorRfqApiService::new(orchestrator));
    router_with_service(runtime, service)
}

/// Build a router with an injected RFQ service implementation.
pub fn router_with_service(runtime: RuntimeHandle, service: Arc<dyn RfqApiService>) -> Router {
    let context = Arc::new(ApiContext { runtime, service });

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

/// Serve the local HTTP API using the live runtime orchestrator.
///
/// # Errors
///
/// Returns an error when the configured address is invalid, the socket cannot
/// bind, or the HTTP server exits with an error.
pub async fn serve_orchestrator(orchestrator: Arc<RuntimeOrchestrator>) -> AppResult<()> {
    let address = bind_address(orchestrator.config())?;
    let router = router_with_orchestrator(orchestrator);
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
}

#[derive(Debug, Clone)]
struct DisabledRfqApiService {
    message: Arc<str>,
}

impl DisabledRfqApiService {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: Arc::<str>::from(message.into()),
        }
    }
}

#[async_trait]
impl RfqApiService for DisabledRfqApiService {
    async fn request_quote(&self, request: RfqRequest) -> Result<RfqResponse, ApiError> {
        let _ = request;
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            self.message.as_ref(),
            Vec::new(),
        ))
    }

    async fn accept_quote(&self, quote_id: QuoteId) -> Result<QuoteAcceptResponse, ApiError> {
        let _ = quote_id;
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            self.message.as_ref(),
            Vec::new(),
        ))
    }

    async fn trade(&self, trade_id: TradeId) -> Result<TradeResponse, ApiError> {
        let _ = trade_id;
        Err(ApiError::not_found(
            "trade_not_found",
            "trade is not available",
        ))
    }
}

#[derive(Clone)]
struct OrchestratorRfqApiService {
    orchestrator: Arc<RuntimeOrchestrator>,
}

impl OrchestratorRfqApiService {
    fn new(orchestrator: Arc<RuntimeOrchestrator>) -> Self {
        Self { orchestrator }
    }
}

impl std::fmt::Debug for OrchestratorRfqApiService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OrchestratorRfqApiService")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl RfqApiService for OrchestratorRfqApiService {
    async fn request_quote(&self, request: RfqRequest) -> Result<RfqResponse, ApiError> {
        let output_mint = request.output_mint.clone();
        let response = self
            .orchestrator
            .request_rfq(api_to_domain_rfq(request))
            .await
            .map_err(ApiError::from_app_error)?;
        Ok(domain_to_api_rfq(response, output_mint))
    }

    async fn accept_quote(&self, quote_id: QuoteId) -> Result<QuoteAcceptResponse, ApiError> {
        let trade = self
            .orchestrator
            .accept_quote(quote_id)
            .await
            .map_err(ApiError::from_app_error)?;
        let ledger_summary = ledger_summary_to_api(
            &self
                .orchestrator
                .ledger_summary()
                .map_err(ApiError::from_app_error)?,
        );

        Ok(QuoteAcceptResponse {
            quote_id: trade.quote_id,
            trade_id: trade.trade_id,
            settlement_status: trade.settlement_status,
            tx_signatures: trade.tx_signatures,
            ledger_summary,
            integration_status: IntegrationStatus::RuntimeOrchestrated,
        })
    }

    async fn trade(&self, trade_id: TradeId) -> Result<TradeResponse, ApiError> {
        let trade = self
            .orchestrator
            .trade(trade_id)
            .await
            .ok_or_else(|| ApiError::not_found("trade_not_found", "trade was not found"))?;
        let ledger_summary = ledger_summary_to_api(
            &self
                .orchestrator
                .ledger_summary()
                .map_err(ApiError::from_app_error)?,
        );
        Ok(trade_to_api_response(trade, ledger_summary))
    }
}

async fn post_rfq(
    State(context): State<Arc<ApiContext>>,
    payload: Result<Json<RfqRequest>, JsonRejection>,
) -> Result<Response<Body>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::from_json_rejection(&error))?;
    let response = context.service.request_quote(request).await?;
    Ok(Json(response).into_response())
}

async fn post_quote_accept(
    State(context): State<Arc<ApiContext>>,
    path: Result<Path<QuoteId>, PathRejection>,
) -> Result<Response<Body>, ApiError> {
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

    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message, Vec::new())
    }

    fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message, Vec::new())
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

fn bind_address(config: &AppConfig) -> AppResult<SocketAddr> {
    format!("{}:{}", config.http.bind_address, config.http.port)
        .parse()
        .map_err(|error| AppError::validation(format!("invalid HTTP bind address: {error}")))
}

fn api_to_domain_rfq(request: RfqRequest) -> rfq::RfqRequest {
    rfq::RfqRequest {
        input_mint: request.input_mint,
        output_mint: request.output_mint,
        input_amount_raw: request.input_amount_raw,
        taker_wallet: request.taker_wallet,
        expiry_seconds: request.expiry_seconds,
    }
}

fn domain_to_api_rfq(response: rfq::RfqResponse, output_mint: MintAddress) -> RfqResponse {
    match response {
        rfq::RfqResponse::Accepted(quote) => {
            let risk_checks = match &quote.risk_decision {
                crate::domain::types::RiskDecision::Accepted { checks } => checks.clone(),
                crate::domain::types::RiskDecision::Rejected { details, .. } => details.clone(),
            };
            RfqResponse::Accepted {
                quote_id: quote.quote_id,
                quoted_output_amount_raw: quote.output_amount.amount_raw,
                spread_bps: u16::try_from(quote.spread_bps.max(0)).unwrap_or(u16::MAX),
                expires_at: quote.expires_at,
                htlc_terms: HtlcAcceptanceTerms {
                    settlement_model: "solana_htlc".to_owned(),
                    escrow_mint: output_mint,
                    expires_at: quote.htlc_terms.expires_at,
                    hashlock: None,
                },
                risk_checks,
                integration_status: IntegrationStatus::RuntimeOrchestrated,
            }
        }
        rfq::RfqResponse::Rejected(rejection) => {
            let risk_check_details = match rejection.decision {
                crate::domain::types::RiskDecision::Accepted { checks } => checks,
                crate::domain::types::RiskDecision::Rejected { details, .. } => details,
            };
            RfqResponse::Rejected {
                reason: rejection.reason,
                risk_check_details,
                integration_status: IntegrationStatus::RuntimeOrchestrated,
            }
        }
    }
}

fn trade_to_api_response(trade: RuntimeTrade, ledger_summary: LedgerSummary) -> TradeResponse {
    TradeResponse {
        trade_id: trade.trade_id,
        settlement_status: trade.settlement_status,
        tx_signatures: trade.tx_signatures,
        amounts: TradeAmounts {
            input: Some(trade.input_amount),
            output: Some(trade.output_amount),
        },
        ledger_summary,
        integration_status: IntegrationStatus::RuntimeOrchestrated,
    }
}

fn ledger_summary_to_api(
    summary: &crate::application::runtime::RuntimeLedgerSummary,
) -> LedgerSummary {
    LedgerSummary {
        balanced: summary.balanced,
        entry_count: summary.entry_count,
        net_usdc_estimate: summary.net_usdc_estimate,
    }
}
