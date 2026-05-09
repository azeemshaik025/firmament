//! Local Axum operator API for RFQ route shells and runtime projection reads.

pub mod auth;
mod observability;
pub mod runtime_read;
pub mod types;

use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::{Body, Bytes};
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_decimal::Decimal;
use serde::Deserialize;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};

use crate::application::rfq;
use crate::application::runtime::{
    AppState, RuntimeHandle, RuntimeOrchestrator, RuntimePersistence, RuntimeTrade,
    WalletSettlementResume, WalletTakerRefundResult,
};
use crate::config::{
    AppConfig, AssetConfig, FIRMAMENT_ADMIN_PASSWORD_HASHES_ENV, FIRMAMENT_ADMIN_SESSION_SECRET_ENV,
};
use crate::domain::assets::{AssetError, AssetRegistry, SOL_ID};
use crate::domain::settlement::SettlementPhase;
use crate::domain::types::{
    AmountRaw, AssetId, MintAddress, QuoteId, RejectionReason, SettlementStatus, TradeId,
};
use crate::error::{AppError, AppResult};
use crate::interfaces::http::auth::{
    parse_admin_password_hashes, sign_admin_session_cookie, validate_admin_session_cookie,
    verify_admin_password,
};
use crate::interfaces::http::types::{
    AbandonRequest, AdminLoginRequest, AdminMeResponse, AdminSummaryResponse, AmountView,
    AssetResponse, ErrorBody, ErrorResponse, HtlcAcceptanceTerms, IntegrationStatus, LedgerSummary,
    NextAction, PairResponse, PairsResponse, QuoteAcceptResponse, RfqPair, RfqRequest, RfqResponse,
    RuntimeEventsResponse, RuntimeStateResponse, TakerLockRequest, TakerLockResponse,
    TakerRedeemRequest, TakerRedeemResponse, TakerRefundRequest, TakerRefundResponse, TradeAmounts,
    TradeResponse, WalletSettlementRequest, WalletSettlementResponse,
    WalletSettlementResumeResponse,
};
use std::str::FromStr;

const ADMIN_COOKIE_NAME: &str = "firmament_admin";

/// Service boundary used by the API for quote, settlement, and trade reads.
#[async_trait]
pub trait RfqApiService: Send + Sync {
    /// Handle an RFQ request.
    async fn request_quote(&self, request: RfqRequest) -> Result<RfqResponse, ApiError>;

    /// Accept an existing quote and create a trade shell.
    async fn accept_quote(&self, quote_id: QuoteId) -> Result<QuoteAcceptResponse, ApiError>;

    /// Start a connected-wallet settlement for an accepted quote.
    async fn start_wallet_settlement(
        &self,
        quote_id: QuoteId,
        request: WalletSettlementRequest,
    ) -> Result<WalletSettlementResponse, ApiError>;

    /// Record the connected-wallet taker lock signature.
    async fn record_taker_lock(
        &self,
        trade_id: TradeId,
        request: TakerLockRequest,
    ) -> Result<TakerLockResponse, ApiError>;

    /// Prepare or record the connected-wallet taker redeem step.
    async fn taker_redeem(
        &self,
        trade_id: TradeId,
        request: TakerRedeemRequest,
    ) -> Result<TakerRedeemResponse, ApiError>;

    /// Resume an active connected-wallet settlement.
    async fn resume_wallet_settlement(
        &self,
        trade_id: TradeId,
    ) -> Result<WalletSettlementResumeResponse, ApiError>;

    /// Abandon a pre-lock connected-wallet settlement.
    async fn abandon_wallet_settlement(
        &self,
        trade_id: TradeId,
        request: AbandonRequest,
    ) -> Result<TradeResponse, ApiError>;

    /// Prepare or record a connected-wallet taker refund.
    async fn taker_refund(
        &self,
        trade_id: TradeId,
        request: TakerRefundRequest,
    ) -> Result<TakerRefundResponse, ApiError>;

    /// Read the current trade shell.
    async fn trade(&self, trade_id: TradeId) -> Result<TradeResponse, ApiError>;

    /// Trigger an operator-requested rebalance check.
    async fn trigger_rebalance_check(&self) -> Result<(), ApiError>;

    /// Trigger an operator-requested Gateway refill check.
    async fn trigger_gateway_refill_check(&self) -> Result<(), ApiError>;

    /// Trigger an operator-requested excess Gateway deposit check.
    async fn trigger_gateway_deposit_check(&self) -> Result<(), ApiError>;
}

/// Build a read-only router for a bootstrapped runtime projection. Mutating RFQ
/// routes return a clear unavailable error unless an orchestrator-backed
/// service is injected.
pub fn router(app_state: &AppState) -> Router {
    let service = Arc::new(DisabledRfqApiService::new(
        "RFQ orchestrator is not attached to this API router",
    ));
    router_with_service(app_state.config().clone(), app_state.runtime(), service)
}

/// Build a production router backed by the runtime orchestrator.
pub fn router_with_orchestrator(orchestrator: Arc<RuntimeOrchestrator>) -> Router {
    let runtime = orchestrator.runtime();
    let config = orchestrator.config().clone();
    let persistence = orchestrator.persistence();
    let service: Arc<dyn RfqApiService> =
        Arc::new(OrchestratorRfqApiService::new(orchestrator.clone()));
    let context = Arc::new(ApiContext {
        config,
        runtime,
        service,
        persistence,
        orchestrator: Some(orchestrator),
    });
    build_router(context)
}

/// Build a router with an injected RFQ service implementation.
pub fn router_with_service(
    config: AppConfig,
    runtime: RuntimeHandle,
    service: Arc<dyn RfqApiService>,
) -> Router {
    let context = Arc::new(ApiContext {
        config,
        runtime,
        service,
        persistence: None,
        orchestrator: None,
    });
    build_router(context)
}

fn build_router(context: Arc<ApiContext>) -> Router {
    Router::new()
        .route("/health", get(runtime_read::get_health))
        .route("/v1/runtime/ledger", get(runtime_read::get_ledger))
        .route("/v1/runtime/trades", get(runtime_read::get_trades))
        .route("/v1/rfq", post(post_rfq))
        .route("/v1/quotes/{quote_id}/accept", post(post_quote_accept))
        .route(
            "/v1/quotes/{quote_id}/wallet-settlement",
            post(post_wallet_settlement),
        )
        .route("/v1/trades/{trade_id}/taker-lock", post(post_taker_lock))
        .route(
            "/v1/trades/{trade_id}/taker-redeem",
            post(post_taker_redeem),
        )
        .route("/v1/trades/{trade_id}/resume", post(post_trade_resume))
        .route("/v1/trades/{trade_id}/abandon", post(post_trade_abandon))
        .route(
            "/v1/trades/{trade_id}/taker-refund",
            post(post_taker_refund),
        )
        .route("/v1/assets", get(get_assets))
        .route("/v1/pairs", get(get_pairs))
        .route("/v1/trades/{trade_id}", get(get_trade))
        .route("/v1/runtime/state", get(get_runtime_state))
        .route("/v1/runtime/events", get(get_runtime_events))
        .route("/v1/admin/login", post(post_admin_login))
        .route("/v1/admin/logout", post(post_admin_logout))
        .route("/v1/admin/me", get(get_admin_me))
        .route("/v1/admin/summary", get(get_admin_summary))
        .route(
            "/v1/admin/rebalance/check",
            post(post_admin_rebalance_check),
        )
        .route(
            "/v1/admin/gateway/refill/check",
            post(post_admin_gateway_refill_check),
        )
        .route(
            "/v1/admin/gateway/deposit/check",
            post(post_admin_gateway_deposit_check),
        )
        .layer(docs_playground_cors())
        .layer(middleware::from_fn(observability::log_request))
        .with_state(context)
}

fn docs_playground_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
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
pub(crate) struct ApiContext {
    pub(crate) config: AppConfig,
    pub(crate) runtime: RuntimeHandle,
    pub(crate) service: Arc<dyn RfqApiService>,
    /// Durable persistence layer when wired (orchestrator router only).
    pub(crate) persistence: Option<Arc<RuntimePersistence>>,
    /// Live orchestrator handle when wired (orchestrator router only).
    pub(crate) orchestrator: Option<Arc<RuntimeOrchestrator>>,
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

    async fn start_wallet_settlement(
        &self,
        quote_id: QuoteId,
        request: WalletSettlementRequest,
    ) -> Result<WalletSettlementResponse, ApiError> {
        let _ = quote_id;
        let _ = request;
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            self.message.as_ref(),
            Vec::new(),
        ))
    }

    async fn record_taker_lock(
        &self,
        trade_id: TradeId,
        request: TakerLockRequest,
    ) -> Result<TakerLockResponse, ApiError> {
        let _ = trade_id;
        let _ = request;
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            self.message.as_ref(),
            Vec::new(),
        ))
    }

    async fn taker_redeem(
        &self,
        trade_id: TradeId,
        request: TakerRedeemRequest,
    ) -> Result<TakerRedeemResponse, ApiError> {
        let _ = trade_id;
        let _ = request;
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            self.message.as_ref(),
            Vec::new(),
        ))
    }

    async fn resume_wallet_settlement(
        &self,
        trade_id: TradeId,
    ) -> Result<WalletSettlementResumeResponse, ApiError> {
        let _ = trade_id;
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            self.message.as_ref(),
            Vec::new(),
        ))
    }

    async fn abandon_wallet_settlement(
        &self,
        trade_id: TradeId,
        request: AbandonRequest,
    ) -> Result<TradeResponse, ApiError> {
        let _ = trade_id;
        let _ = request;
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            self.message.as_ref(),
            Vec::new(),
        ))
    }

    async fn taker_refund(
        &self,
        trade_id: TradeId,
        request: TakerRefundRequest,
    ) -> Result<TakerRefundResponse, ApiError> {
        let _ = trade_id;
        let _ = request;
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

    async fn trigger_rebalance_check(&self) -> Result<(), ApiError> {
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            self.message.as_ref(),
            Vec::new(),
        ))
    }

    async fn trigger_gateway_refill_check(&self) -> Result<(), ApiError> {
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            self.message.as_ref(),
            Vec::new(),
        ))
    }

    async fn trigger_gateway_deposit_check(&self) -> Result<(), ApiError> {
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            self.message.as_ref(),
            Vec::new(),
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
        let config = self.orchestrator.config();
        let domain_request = parse_rfq_request(request, config)?;
        info!(
            input_mint = %domain_request.input_mint,
            output_mint = %domain_request.output_mint,
            input_amount_raw = domain_request.input_amount_raw.as_u64(),
            "RFQ requested"
        );
        let fallback_output_mint = domain_request.output_mint.clone();
        let response = self
            .orchestrator
            .request_rfq(domain_request)
            .await
            .map_err(ApiError::from_app_error)?;
        match &response {
            rfq::RfqResponse::Accepted(quote) => {
                info!(
                    quote_id = %quote.quote_id,
                    input_asset = %quote.pair.input,
                    output_asset = %quote.pair.output,
                    input_amount_raw = quote.input_amount.amount_raw.as_u64(),
                    output_amount_raw = quote.output_amount.amount_raw.as_u64(),
                    spread_bps = quote.spread_bps,
                    execution_path = ?quote.execution_path,
                    "RFQ accepted"
                );
            }
            rfq::RfqResponse::Rejected(rejection) => {
                let quote_id = rejection
                    .quote_id
                    .map_or_else(|| "unassigned".to_owned(), |quote_id| quote_id.to_string());
                warn!(
                    quote_id = %quote_id,
                    reason = ?rejection.reason,
                    "RFQ rejected"
                );
            }
        }
        Ok(domain_to_api_rfq(response, fallback_output_mint, config))
    }

    async fn accept_quote(&self, quote_id: QuoteId) -> Result<QuoteAcceptResponse, ApiError> {
        let _ = quote_id;
        Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "wallet_settlement_required",
            "server-side taker settlement is disabled; use /v1/quotes/{quote_id}/wallet-settlement",
            Vec::new(),
        ))
    }

    async fn start_wallet_settlement(
        &self,
        quote_id: QuoteId,
        request: WalletSettlementRequest,
    ) -> Result<WalletSettlementResponse, ApiError> {
        let response = self
            .orchestrator
            .start_wallet_settlement(quote_id, request.taker_wallet, request.secret_hash)
            .await
            .map_err(ApiError::from_app_error)?;
        info!(
            quote_id = %response.quote_id,
            trade_id = %response.trade_id,
            expires_at = %response.expires_at,
            "wallet settlement started"
        );

        let next_action = NextAction {
            kind: "submit_taker_lock".to_owned(),
            method: "POST".to_owned(),
            path: format!("/v1/trades/{}/taker-lock", response.trade_id),
        };
        Ok(WalletSettlementResponse {
            quote_id: response.quote_id,
            trade_id: response.trade_id,
            taker_lock_transaction: response.taker_lock_transaction,
            expires_at: response.expires_at,
            integration_status: IntegrationStatus::RuntimeOrchestrated,
            next_action,
        })
    }

    async fn record_taker_lock(
        &self,
        trade_id: TradeId,
        request: TakerLockRequest,
    ) -> Result<TakerLockResponse, ApiError> {
        let response = self
            .orchestrator
            .record_wallet_taker_lock(trade_id, request.signature)
            .await
            .map_err(ApiError::from_app_error)?;
        info!(
            trade_id = %response.trade_id,
            settlement_status = ?response.settlement_status,
            maker_lock_submitted = response.maker_lock_signature.is_some(),
            signature_count = response.tx_signatures.len(),
            "taker lock recorded"
        );

        let next_action = NextAction {
            kind: "submit_taker_redeem".to_owned(),
            method: "POST".to_owned(),
            path: format!("/v1/trades/{}/taker-redeem", response.trade_id),
        };
        Ok(TakerLockResponse {
            trade_id: response.trade_id,
            settlement_status: response.settlement_status,
            maker_lock_signature: response.maker_lock_signature,
            tx_signatures: response.tx_signatures,
            integration_status: IntegrationStatus::RuntimeOrchestrated,
            next_action,
        })
    }

    async fn taker_redeem(
        &self,
        trade_id: TradeId,
        request: TakerRedeemRequest,
    ) -> Result<TakerRedeemResponse, ApiError> {
        let ledger_summary = || {
            self.orchestrator
                .ledger_summary()
                .map(|summary| ledger_summary_to_api(&summary))
                .map_err(ApiError::from_app_error)
        };

        let Some(signature) = request.signature else {
            let prepared = self
                .orchestrator
                .prepare_wallet_taker_redeem(trade_id, request.preimage)
                .await
                .map_err(ApiError::from_app_error)?;
            info!(
                trade_id = %prepared.trade_id,
                "taker redeem transaction prepared"
            );
            let next_action = NextAction {
                kind: "submit_taker_redeem".to_owned(),
                method: "POST".to_owned(),
                path: format!("/v1/trades/{}/taker-redeem", prepared.trade_id),
            };
            return Ok(TakerRedeemResponse {
                trade_id: prepared.trade_id,
                settlement_status: SettlementStatus::Initiated,
                taker_redeem_transaction: Some(prepared.taker_redeem_transaction),
                maker_redeem_signature: None,
                tx_signatures: Vec::new(),
                ledger_summary: ledger_summary()?,
                integration_status: IntegrationStatus::RuntimeOrchestrated,
                next_action: Some(next_action),
            });
        };

        let completed = self
            .orchestrator
            .complete_wallet_taker_redeem(trade_id, request.preimage, signature)
            .await
            .map_err(ApiError::from_app_error)?;
        info!(
            trade_id = %completed.trade.trade_id,
            settlement_status = ?completed.trade.settlement_status,
            maker_redeem_submitted = completed.maker_redeem_signature.is_some(),
            signature_count = completed.trade.tx_signatures.len(),
            "taker redeem completed"
        );
        Ok(TakerRedeemResponse {
            trade_id: completed.trade.trade_id,
            settlement_status: completed.trade.settlement_status,
            taker_redeem_transaction: None,
            maker_redeem_signature: completed.maker_redeem_signature,
            tx_signatures: completed.trade.tx_signatures,
            ledger_summary: ledger_summary()?,
            integration_status: IntegrationStatus::RuntimeOrchestrated,
            next_action: None,
        })
    }

    async fn resume_wallet_settlement(
        &self,
        trade_id: TradeId,
    ) -> Result<WalletSettlementResumeResponse, ApiError> {
        let response = self
            .orchestrator
            .resume_wallet_settlement(trade_id)
            .await
            .map_err(ApiError::from_app_error)?;
        Ok(wallet_resume_to_api_response(response))
    }

    async fn abandon_wallet_settlement(
        &self,
        trade_id: TradeId,
        request: AbandonRequest,
    ) -> Result<TradeResponse, ApiError> {
        let trade = self
            .orchestrator
            .abandon_wallet_settlement(trade_id, request.secret_hash)
            .await
            .map_err(ApiError::from_app_error)?;
        let ledger_summary = ledger_summary_to_api(
            &self
                .orchestrator
                .ledger_summary()
                .map_err(ApiError::from_app_error)?,
        );
        Ok(trade_to_api_response(
            trade,
            ledger_summary,
            self.orchestrator.config(),
            self.orchestrator.run_id(),
        ))
    }

    async fn taker_refund(
        &self,
        trade_id: TradeId,
        request: TakerRefundRequest,
    ) -> Result<TakerRefundResponse, ApiError> {
        let ledger_summary = || {
            self.orchestrator
                .ledger_summary()
                .map(|summary| ledger_summary_to_api(&summary))
                .map_err(ApiError::from_app_error)
        };

        let Some(signature) = request.signature else {
            let prepared = self
                .orchestrator
                .prepare_wallet_taker_refund(trade_id)
                .await
                .map_err(ApiError::from_app_error)?;
            let next_action = NextAction {
                kind: "submit_taker_refund".to_owned(),
                method: "POST".to_owned(),
                path: format!("/v1/trades/{}/taker-refund", prepared.trade_id),
            };
            return Ok(TakerRefundResponse {
                trade_id: prepared.trade_id,
                settlement_status: SettlementStatus::Initiated,
                taker_refund_transaction: Some(prepared.taker_refund_transaction),
                maker_refund_signature: None,
                tx_signatures: Vec::new(),
                ledger_summary: ledger_summary()?,
                integration_status: IntegrationStatus::RuntimeOrchestrated,
                next_action: Some(next_action),
            });
        };

        let completed = self
            .orchestrator
            .complete_wallet_taker_refund(trade_id, signature)
            .await
            .map_err(ApiError::from_app_error)?;
        Ok(taker_refund_to_api_response(completed, ledger_summary()?))
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
        Ok(trade_to_api_response(
            trade,
            ledger_summary,
            self.orchestrator.config(),
            self.orchestrator.run_id(),
        ))
    }

    async fn trigger_rebalance_check(&self) -> Result<(), ApiError> {
        self.orchestrator
            .trigger_operator_automation_check("rebalance")
            .await
            .map_err(ApiError::from_app_error)?;
        info!("operator rebalance check triggered");
        Ok(())
    }

    async fn trigger_gateway_refill_check(&self) -> Result<(), ApiError> {
        self.orchestrator
            .trigger_operator_automation_check("gateway")
            .await
            .map_err(ApiError::from_app_error)?;
        info!("operator Gateway refill check triggered");
        Ok(())
    }

    async fn trigger_gateway_deposit_check(&self) -> Result<(), ApiError> {
        self.orchestrator
            .run_excess_deposit_check()
            .await
            .map(|_| ())
            .map_err(ApiError::from_app_error)?;
        info!("operator Gateway deposit check triggered");
        Ok(())
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

async fn post_wallet_settlement(
    State(context): State<Arc<ApiContext>>,
    path: Result<Path<QuoteId>, PathRejection>,
    payload: Result<Json<WalletSettlementRequest>, JsonRejection>,
) -> Result<Response<Body>, ApiError> {
    let Path(quote_id) = path.map_err(|error| ApiError::from_path_rejection(&error))?;
    let Json(request) = payload.map_err(|error| ApiError::from_json_rejection(&error))?;
    let response = context
        .service
        .start_wallet_settlement(quote_id, request)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

async fn post_taker_lock(
    State(context): State<Arc<ApiContext>>,
    path: Result<Path<TradeId>, PathRejection>,
    payload: Result<Json<TakerLockRequest>, JsonRejection>,
) -> Result<Response<Body>, ApiError> {
    let Path(trade_id) = path.map_err(|error| ApiError::from_path_rejection(&error))?;
    let Json(request) = payload.map_err(|error| ApiError::from_json_rejection(&error))?;
    let response = context.service.record_taker_lock(trade_id, request).await?;
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

async fn post_taker_redeem(
    State(context): State<Arc<ApiContext>>,
    path: Result<Path<TradeId>, PathRejection>,
    payload: Result<Json<TakerRedeemRequest>, JsonRejection>,
) -> Result<Response<Body>, ApiError> {
    let Path(trade_id) = path.map_err(|error| ApiError::from_path_rejection(&error))?;
    let Json(request) = payload.map_err(|error| ApiError::from_json_rejection(&error))?;
    let response = context.service.taker_redeem(trade_id, request).await?;
    let status = if response.taker_redeem_transaction.is_some() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((status, Json(response)).into_response())
}

async fn post_trade_resume(
    State(context): State<Arc<ApiContext>>,
    path: Result<Path<TradeId>, PathRejection>,
) -> Result<Response<Body>, ApiError> {
    let Path(trade_id) = path.map_err(|error| ApiError::from_path_rejection(&error))?;
    let response = context.service.resume_wallet_settlement(trade_id).await?;
    Ok(Json(response).into_response())
}

async fn post_trade_abandon(
    State(context): State<Arc<ApiContext>>,
    path: Result<Path<TradeId>, PathRejection>,
    payload: Result<Json<AbandonRequest>, JsonRejection>,
) -> Result<Response<Body>, ApiError> {
    let Path(trade_id) = path.map_err(|error| ApiError::from_path_rejection(&error))?;
    let Json(request) = payload.map_err(|error| ApiError::from_json_rejection(&error))?;
    let response = context
        .service
        .abandon_wallet_settlement(trade_id, request)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

async fn post_taker_refund(
    State(context): State<Arc<ApiContext>>,
    path: Result<Path<TradeId>, PathRejection>,
    body: Bytes,
) -> Result<Response<Body>, ApiError> {
    let Path(trade_id) = path.map_err(|error| ApiError::from_path_rejection(&error))?;
    let request = if body.is_empty() {
        TakerRefundRequest::default()
    } else {
        serde_json::from_slice::<TakerRefundRequest>(&body)
            .map_err(|error| ApiError::bad_request("invalid_json", error.to_string()))?
    };
    let response = context.service.taker_refund(trade_id, request).await?;
    let status = if response.taker_refund_transaction.is_some() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((status, Json(response)).into_response())
}

async fn get_assets(
    State(context): State<Arc<ApiContext>>,
) -> Result<Json<Vec<AssetResponse>>, ApiError> {
    Ok(Json(build_assets_response(&context.config)))
}

async fn get_pairs(
    State(context): State<Arc<ApiContext>>,
) -> Result<Json<PairsResponse>, ApiError> {
    Ok(Json(build_pairs_response(&context.config)))
}

fn build_assets_response(config: &AppConfig) -> Vec<AssetResponse> {
    let network = solana_network_label(&config.solana.cluster);
    config
        .assets
        .supported
        .iter()
        .filter(|asset| asset.enabled)
        .map(|asset| {
            let aliases = asset_aliases(asset);
            let kind = if asset.id.as_str() == SOL_ID {
                "native".to_owned()
            } else {
                "spl".to_owned()
            };
            let supported_outputs = config
                .assets
                .enabled_pairs()
                .into_iter()
                .filter(|pair| pair.input == asset.id)
                .map(|pair| pair.output.to_string())
                .collect::<Vec<_>>();
            let quoteable_threshold_raw = asset.quoteable_threshold_raw.as_u64().to_string();
            let quoteable_threshold =
                raw_to_display_string(asset.quoteable_threshold_raw, asset.decimals);
            AssetResponse {
                id: asset.id.to_string(),
                symbol: asset.symbol.clone(),
                mint: asset.mint.clone(),
                decimals: asset.decimals,
                min_trade_amount: asset.min_trade_amount,
                max_trade_amount: asset.max_trade_amount,
                aliases,
                kind,
                network: network.clone(),
                supported_outputs,
                quoteable_threshold_raw,
                quoteable_threshold,
            }
        })
        .collect()
}

fn build_pairs_response(config: &AppConfig) -> PairsResponse {
    let by_id: std::collections::HashMap<&AssetId, &AssetConfig> = config
        .assets
        .supported
        .iter()
        .filter(|asset| asset.enabled)
        .map(|asset| (&asset.id, asset))
        .collect();
    let pairs = config
        .assets
        .enabled_pairs()
        .into_iter()
        .filter_map(|pair| {
            let input = by_id.get(&pair.input)?;
            let output = by_id.get(&pair.output)?;
            Some(PairResponse {
                input_asset: input.id.to_string(),
                output_asset: output.id.to_string(),
                input_mint: input.mint.clone(),
                output_mint: output.mint.clone(),
                input_decimals: input.decimals,
                output_decimals: output.decimals,
                max_quote_notional_usd: config.risk.max_quote_notional_usd,
                min_quote_notional_usd: Decimal::ZERO,
                min_input_trade_amount: input.min_trade_amount,
                max_input_trade_amount: input.max_trade_amount,
                default_expiry_seconds: config.assets.policy.default_quote_expiry_seconds,
            })
        })
        .collect();
    PairsResponse { pairs }
}

fn solana_network_label(cluster: &str) -> String {
    let trimmed = cluster.trim();
    let normalized = trimmed.to_ascii_lowercase();
    if normalized == "mainnet-beta" || normalized == "mainnet" {
        "solana-mainnet-beta".to_owned()
    } else if normalized.starts_with("solana-") {
        normalized
    } else {
        format!("solana-{normalized}")
    }
}

fn asset_aliases(asset: &AssetConfig) -> Vec<String> {
    let mut aliases = Vec::new();
    let id_lower = asset.id.as_str().to_ascii_lowercase();
    let symbol_lower = asset.symbol.to_ascii_lowercase();
    aliases.push(id_lower.clone());
    if symbol_lower != id_lower {
        aliases.push(symbol_lower);
    }
    aliases
}

fn raw_to_display_string(amount: AmountRaw, decimals: u8) -> String {
    if decimals == 0 {
        return amount.as_u64().to_string();
    }
    let raw = amount.as_u64();
    let mut scale: u64 = 1;
    for _ in 0..decimals {
        match scale.checked_mul(10) {
            Some(next) => scale = next,
            None => return raw.to_string(),
        }
    }
    let whole = raw / scale;
    let fractional = raw % scale;
    format!(
        "{whole}.{fractional:0width$}",
        width = usize::from(decimals)
    )
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

async fn post_admin_login(
    State(context): State<Arc<ApiContext>>,
    payload: Result<Json<AdminLoginRequest>, JsonRejection>,
) -> Result<Response<Body>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::from_json_rejection(&error))?;
    let username = request.username.trim();
    if !context
        .config
        .admin
        .allowed_users
        .iter()
        .any(|allowed| allowed == username)
    {
        warn!(username, "admin login rejected: username is not allowed");
        return Err(ApiError::unauthorized());
    }

    let hashes = admin_password_hashes()?;
    if !verify_admin_password(username, &request.password, &hashes)
        .map_err(ApiError::from_app_error)?
    {
        warn!(
            username,
            "admin login rejected: password verification failed"
        );
        return Err(ApiError::unauthorized());
    }

    let ttl_seconds = i64::try_from(context.config.admin.session_ttl_seconds).map_err(|_| {
        ApiError::bad_request(
            "invalid_admin_session_ttl",
            "admin session TTL is too large",
        )
    })?;
    let expires_at = time::OffsetDateTime::now_utc() + time::Duration::seconds(ttl_seconds);
    let cookie_value = sign_admin_session_cookie(username, expires_at, &admin_session_secret()?)
        .map_err(ApiError::from_app_error)?;
    let cookie = session_cookie_header(
        &cookie_value,
        context.config.admin.session_ttl_seconds,
        context.config.admin.cookie_secure,
    )?;

    let mut response = Json(AdminMeResponse {
        authenticated: true,
        username: Some(username.to_owned()),
        expires_at: Some(expires_at),
    })
    .into_response();
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    info!(username, expires_at = %expires_at, "admin login succeeded");
    Ok(response)
}

async fn post_admin_logout() -> Result<Response<Body>, ApiError> {
    let mut response = Json(AdminMeResponse {
        authenticated: false,
        username: None,
        expires_at: None,
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{ADMIN_COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"
        ))
        .map_err(|error| ApiError::bad_request("invalid_cookie", error.to_string()))?,
    );
    info!("admin logout completed");
    Ok(response)
}

async fn get_admin_me(
    State(context): State<Arc<ApiContext>>,
    headers: HeaderMap,
) -> Result<Json<AdminMeResponse>, ApiError> {
    let Some(session) = optional_admin_session(&headers, &context.config)? else {
        return Ok(Json(AdminMeResponse {
            authenticated: false,
            username: None,
            expires_at: None,
        }));
    };

    Ok(Json(AdminMeResponse {
        authenticated: true,
        username: Some(session.username),
        expires_at: Some(session.expires_at),
    }))
}

async fn get_admin_summary(
    State(context): State<Arc<ApiContext>>,
    headers: HeaderMap,
) -> Result<Json<AdminSummaryResponse>, ApiError> {
    require_admin_session(&headers, &context.config)?;
    let state = context.runtime.snapshot().await;
    let events = context.runtime.recent_events(Some(30)).await;
    Ok(Json(AdminSummaryResponse {
        inventory: state.inventory,
        risk: state.risk,
        pnl: state.pnl,
        rfq: state.rfq,
        rebalance: state.rebalance,
        gateway: state.gateway,
        recent_events: events,
    }))
}

async fn post_admin_rebalance_check(
    State(context): State<Arc<ApiContext>>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_admin_session(&headers, &context.config)?;
    context.service.trigger_rebalance_check().await?;
    Ok(StatusCode::ACCEPTED)
}

async fn post_admin_gateway_refill_check(
    State(context): State<Arc<ApiContext>>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_admin_session(&headers, &context.config)?;
    context.service.trigger_gateway_refill_check().await?;
    Ok(StatusCode::ACCEPTED)
}

async fn post_admin_gateway_deposit_check(
    State(context): State<Arc<ApiContext>>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_admin_session(&headers, &context.config)?;
    context.service.trigger_gateway_deposit_check().await?;
    Ok(StatusCode::ACCEPTED)
}

fn admin_password_hashes() -> Result<std::collections::HashMap<String, String>, ApiError> {
    let raw = env::var(FIRMAMENT_ADMIN_PASSWORD_HASHES_ENV).map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_auth_unconfigured",
            "admin password hashes are not configured",
            vec![format!("set {FIRMAMENT_ADMIN_PASSWORD_HASHES_ENV}")],
        )
    })?;
    parse_admin_password_hashes(&raw).map_err(ApiError::from_app_error)
}

fn admin_session_secret() -> Result<Vec<u8>, ApiError> {
    env::var(FIRMAMENT_ADMIN_SESSION_SECRET_ENV)
        .ok()
        .map(String::into_bytes)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "admin_auth_unconfigured",
                "admin session secret is not configured",
                vec![format!("set {FIRMAMENT_ADMIN_SESSION_SECRET_ENV}")],
            )
        })
}

fn optional_admin_session(
    headers: &HeaderMap,
    config: &AppConfig,
) -> Result<Option<auth::AdminSession>, ApiError> {
    let Some(cookie_value) = cookie_value(headers, ADMIN_COOKIE_NAME) else {
        return Ok(None);
    };
    let Some(session) = validate_admin_session_cookie(
        cookie_value,
        &admin_session_secret()?,
        time::OffsetDateTime::now_utc(),
    )
    .map_err(ApiError::from_app_error)?
    else {
        return Ok(None);
    };
    if !config
        .admin
        .allowed_users
        .iter()
        .any(|allowed| allowed == &session.username)
    {
        return Ok(None);
    }

    Ok(Some(session))
}

fn require_admin_session(
    headers: &HeaderMap,
    config: &AppConfig,
) -> Result<auth::AdminSession, ApiError> {
    optional_admin_session(headers, config)?.ok_or_else(ApiError::unauthorized)
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(cookie_name, value)| (cookie_name == name).then_some(value))
}

fn session_cookie_header(
    value: &str,
    max_age_seconds: u64,
    secure: bool,
) -> Result<HeaderValue, ApiError> {
    let secure_attr = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{ADMIN_COOKIE_NAME}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age_seconds}{secure_attr}"
    ))
    .map_err(|error| ApiError::bad_request("invalid_cookie", error.to_string()))
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
    pub(crate) fn new(
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

    pub(crate) fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message, Vec::new())
    }

    fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message, Vec::new())
    }

    fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "admin_auth_failed",
            "admin authentication failed",
            Vec::new(),
        )
    }

    fn from_json_rejection(error: &JsonRejection) -> Self {
        Self::bad_request("invalid_json", error.body_text())
    }

    fn from_path_rejection(error: &PathRejection) -> Self {
        Self::bad_request("invalid_path", error.body_text())
    }

    pub(crate) fn from_app_error(error: AppError) -> Self {
        match &error {
            AppError::Validation(_) | AppError::Unsupported(_) | AppError::Conflict(_) => {
                warn!(%error, "API request rejected by application layer");
            }
            AppError::ExternalService { service, .. } => {
                warn!(%service, %error, "API external dependency failed");
            }
            AppError::Config(_)
            | AppError::Solana(_)
            | AppError::Persistence(_)
            | AppError::Internal(_) => {
                error!(%error, "API request failed");
            }
        }

        match error {
            AppError::Validation(message) => Self::bad_request("validation_failed", message),
            AppError::Conflict(message) => {
                Self::new(StatusCode::CONFLICT, "state_conflict", message, Vec::new())
            }
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

#[allow(clippy::too_many_lines)]
fn parse_rfq_request(request: RfqRequest, config: &AppConfig) -> Result<rfq::RfqRequest, ApiError> {
    let RfqRequest {
        input_asset,
        output_asset,
        amount,
        input_mint,
        output_mint,
        input_amount_raw,
        taker_wallet,
        expiry_seconds,
    } = request;

    let used_friendly = input_asset.is_some() || output_asset.is_some() || amount.is_some();
    let used_legacy = input_mint.is_some() || output_mint.is_some() || input_amount_raw.is_some();

    if used_friendly && used_legacy {
        return Err(ApiError::bad_request(
            "invalid_rfq",
            "do not mix friendly (input_asset/output_asset/amount) and legacy (input_mint/output_mint/input_amount_raw) RFQ fields",
        ));
    }

    if used_friendly {
        let input_id = input_asset
            .as_deref()
            .ok_or_else(|| ApiError::bad_request("invalid_rfq", "input_asset is required"))?;
        let output_id = output_asset
            .as_deref()
            .ok_or_else(|| ApiError::bad_request("invalid_rfq", "output_asset is required"))?;
        let raw_amount = amount.as_deref().ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_amount",
                "amount is required",
                Vec::new(),
            )
        })?;

        let input_cfg = resolve_asset_alias(config, input_id)?;
        let output_cfg = resolve_asset_alias(config, output_id)?;
        if input_cfg.id == output_cfg.id {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "same_asset",
                format!(
                    "input and output assets must differ; both resolve to {}",
                    input_cfg.id
                ),
                Vec::new(),
            ));
        }

        let amount_decimal = Decimal::from_str(raw_amount.trim()).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_amount",
                format!("amount '{raw_amount}' is not a valid decimal"),
                Vec::new(),
            )
        })?;
        if amount_decimal <= Decimal::ZERO {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_amount",
                "amount must be greater than zero",
                Vec::new(),
            ));
        }

        let registry = AssetRegistry::from_config(config);
        let raw = registry
            .display_to_raw(&input_cfg.id, amount_decimal)
            .map_err(|err| match err {
                AssetError::PrecisionLoss { .. } => ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_amount",
                    format!(
                        "amount '{raw_amount}' has more than {} decimals (max for {})",
                        input_cfg.decimals, input_cfg.id
                    ),
                    Vec::new(),
                ),
                AssetError::NegativeAmount { .. } => ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_amount",
                    "amount must be greater than zero",
                    Vec::new(),
                ),
                _ => ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_amount",
                    err.to_string(),
                    Vec::new(),
                ),
            })?;

        Ok(rfq::RfqRequest {
            input_mint: input_cfg.mint.clone(),
            output_mint: output_cfg.mint.clone(),
            input_amount_raw: raw,
            taker_wallet,
            expiry_seconds,
        })
    } else {
        let input_mint = input_mint.ok_or_else(|| {
            ApiError::bad_request("invalid_rfq", "input_mint or input_asset is required")
        })?;
        let output_mint = output_mint.ok_or_else(|| {
            ApiError::bad_request("invalid_rfq", "output_mint or output_asset is required")
        })?;
        let input_amount_raw = input_amount_raw.ok_or_else(|| {
            ApiError::bad_request("invalid_rfq", "input_amount_raw or amount is required")
        })?;

        Ok(rfq::RfqRequest {
            input_mint,
            output_mint,
            input_amount_raw,
            taker_wallet,
            expiry_seconds,
        })
    }
}

fn resolve_asset_alias<'a>(
    config: &'a AppConfig,
    identifier: &str,
) -> Result<&'a AssetConfig, ApiError> {
    let needle = identifier.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "unsupported_asset",
            "asset identifier must not be empty",
            Vec::new(),
        ));
    }
    config
        .assets
        .supported
        .iter()
        .find(|asset| {
            asset.enabled
                && (asset.id.as_str().to_ascii_lowercase() == needle
                    || asset.symbol.to_ascii_lowercase() == needle)
        })
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "unsupported_asset",
                format!("unsupported asset '{identifier}'"),
                Vec::new(),
            )
        })
}

fn build_amount_view(asset_id: &AssetId, amount_raw: AmountRaw, config: &AppConfig) -> AmountView {
    let asset_cfg = config
        .assets
        .supported
        .iter()
        .find(|asset| &asset.id == asset_id);
    let decimals = asset_cfg.map_or(0, |asset| asset.decimals);
    let mint = asset_cfg.map(|asset| asset.mint.clone());
    AmountView {
        asset: asset_id.to_string(),
        amount: raw_to_display_string(amount_raw, decimals),
        amount_raw: amount_raw.as_u64().to_string(),
        decimals,
        mint,
    }
}

fn rejection_message(reason: &RejectionReason) -> (&'static str, &'static str) {
    match reason {
        RejectionReason::UnsupportedPair => (
            "This pair is not currently supported.",
            "Try a different pair from /v1/pairs.",
        ),
        RejectionReason::UnsupportedAsset => (
            "This asset is not currently supported.",
            "Use one of the supported assets from /v1/assets.",
        ),
        RejectionReason::AmountTooSmall => (
            "The amount is too small for a quote.",
            "Try a larger amount.",
        ),
        RejectionReason::MaxNotionalExceeded
        | RejectionReason::AboveAssetMaxNotional
        | RejectionReason::AboveAssetMaxAmount
        | RejectionReason::CumulativeCapExceeded => (
            "The amount exceeds a configured maximum.",
            "Try a smaller amount.",
        ),
        RejectionReason::BelowAssetMinNotional | RejectionReason::BelowAssetMinAmount => (
            "The amount is below the per-asset minimum.",
            "Try a larger amount.",
        ),
        RejectionReason::InventoryBelowQuoteableThreshold
        | RejectionReason::ExposureLimitExceeded => (
            "No liquidity sources found.",
            "Try a smaller amount or a different pair.",
        ),
        RejectionReason::StalePrice => (
            "Reference price data is stale.",
            "Try again in a few seconds.",
        ),
        RejectionReason::WalletNotAllowed => (
            "Wallet is not currently allowed.",
            "Contact the operator if you expected access.",
        ),
        RejectionReason::GatewayUnavailable | RejectionReason::ExternalServiceUnavailable => (
            "A required service is temporarily unavailable.",
            "Try again in a few seconds.",
        ),
        RejectionReason::ValidationFailed => (
            "Request validation failed.",
            "Check the request shape and try again.",
        ),
    }
}

fn domain_to_api_rfq(
    response: rfq::RfqResponse,
    fallback_output_mint: MintAddress,
    config: &AppConfig,
) -> RfqResponse {
    match response {
        rfq::RfqResponse::Accepted(quote) => {
            let risk_checks = match &quote.risk_decision {
                crate::domain::types::RiskDecision::Accepted { checks } => checks.clone(),
                crate::domain::types::RiskDecision::Rejected { details, .. } => details.clone(),
            };
            let pair = RfqPair {
                input_asset: quote.pair.input.to_string(),
                output_asset: quote.pair.output.to_string(),
            };
            let input = build_amount_view(
                &quote.input_amount.asset,
                quote.input_amount.amount_raw,
                config,
            );
            let output = build_amount_view(
                &quote.output_amount.asset,
                quote.output_amount.amount_raw,
                config,
            );
            let escrow_mint = output.mint.clone().unwrap_or(fallback_output_mint);
            let next_action = NextAction {
                kind: "start_wallet_settlement".to_owned(),
                method: "POST".to_owned(),
                path: format!("/v1/quotes/{}/wallet-settlement", quote.quote_id),
            };
            RfqResponse::Accepted {
                quote_id: quote.quote_id,
                quoted_output_amount_raw: quote.output_amount.amount_raw,
                spread_bps: u16::try_from(quote.spread_bps.max(0)).unwrap_or(u16::MAX),
                expires_at: quote.expires_at,
                htlc_terms: HtlcAcceptanceTerms {
                    settlement_model: "solana_htlc".to_owned(),
                    escrow_mint,
                    expires_at: quote.htlc_terms.expires_at,
                    hashlock: None,
                },
                risk_checks,
                integration_status: IntegrationStatus::RuntimeOrchestrated,
                pair,
                input,
                output,
                next_action,
            }
        }
        rfq::RfqResponse::Rejected(rejection) => {
            let risk_check_details = match rejection.decision {
                crate::domain::types::RiskDecision::Accepted { checks } => checks,
                crate::domain::types::RiskDecision::Rejected { details, .. } => details,
            };
            let (message, suggested_action) = rejection_message(&rejection.reason);
            RfqResponse::Rejected {
                reason: rejection.reason,
                risk_check_details,
                integration_status: IntegrationStatus::RuntimeOrchestrated,
                message: message.to_owned(),
                suggested_action: suggested_action.to_owned(),
            }
        }
    }
}

fn trade_to_api_response(
    trade: RuntimeTrade,
    ledger_summary: LedgerSummary,
    config: &AppConfig,
    current_run: crate::domain::types::RuntimeRunId,
) -> TradeResponse {
    let input_view = build_amount_view(
        &trade.input_amount.asset,
        trade.input_amount.amount_raw,
        config,
    );
    let output_view = build_amount_view(
        &trade.output_amount.asset,
        trade.output_amount.amount_raw,
        config,
    );
    let created_at = trade.created_at.to_string();
    TradeResponse {
        trade_id: trade.trade_id,
        quote_id: trade.quote_id,
        run_id: trade.run_id.to_string(),
        current_run: trade.run_id == current_run,
        taker_wallet: trade
            .taker_wallet
            .as_ref()
            .map(|wallet| wallet.as_str().to_owned()),
        expires_at: trade.expires_at.map(|expires_at| expires_at.to_string()),
        created_at,
        settlement_status: trade.settlement_status,
        tx_signatures: trade.tx_signatures.clone(),
        tx_signature_kinds: trade.tx_signature_kinds.clone(),
        amounts: TradeAmounts {
            input: Some(trade.input_amount),
            output: Some(trade.output_amount),
        },
        input: Some(input_view),
        output: Some(output_view),
        ledger_summary,
        integration_status: IntegrationStatus::RuntimeOrchestrated,
    }
}

fn wallet_resume_to_api_response(
    response: WalletSettlementResume,
) -> WalletSettlementResumeResponse {
    let next_action = if response.taker_lock_transaction.is_some() {
        Some(NextAction {
            kind: "submit_taker_lock".to_owned(),
            method: "POST".to_owned(),
            path: format!("/v1/trades/{}/taker-lock", response.trade_id),
        })
    } else if response.taker_refund_transaction.is_some() {
        Some(NextAction {
            kind: "submit_taker_refund".to_owned(),
            method: "POST".to_owned(),
            path: format!("/v1/trades/{}/taker-refund", response.trade_id),
        })
    } else {
        Some(NextAction {
            kind: "submit_taker_redeem".to_owned(),
            method: "POST".to_owned(),
            path: format!("/v1/trades/{}/taker-redeem", response.trade_id),
        })
    };
    WalletSettlementResumeResponse {
        trade_id: response.trade_id,
        quote_id: response.quote_id,
        run_id: response.run_id.to_string(),
        settlement_phase: settlement_phase_to_api(response.settlement_phase).to_owned(),
        settlement_status: response.settlement_status,
        taker_lock_transaction: response.taker_lock_transaction,
        taker_redeem_transaction: response.taker_redeem_transaction,
        taker_refund_transaction: response.taker_refund_transaction,
        tx_signatures: response.tx_signatures,
        tx_signature_kinds: response.tx_signature_kinds,
        expires_at: response.expires_at,
        integration_status: IntegrationStatus::RuntimeOrchestrated,
        next_action,
    }
}

fn settlement_phase_to_api(phase: SettlementPhase) -> &'static str {
    match phase {
        SettlementPhase::Pending => "pending",
        SettlementPhase::Started => "started",
        SettlementPhase::TakerLocked => "taker_locked",
        SettlementPhase::MakerLocked => "maker_locked",
        SettlementPhase::TakerRedeemed => "taker_redeemed",
        SettlementPhase::Complete => "complete",
        SettlementPhase::Refunded => "refunded",
        SettlementPhase::Failed => "failed",
    }
}

fn taker_refund_to_api_response(
    completed: WalletTakerRefundResult,
    ledger_summary: LedgerSummary,
) -> TakerRefundResponse {
    TakerRefundResponse {
        trade_id: completed.trade.trade_id,
        settlement_status: completed.trade.settlement_status,
        taker_refund_transaction: None,
        maker_refund_signature: completed.maker_refund_signature,
        tx_signatures: completed.trade.tx_signatures,
        ledger_summary,
        integration_status: IntegrationStatus::RuntimeOrchestrated,
        next_action: None,
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
