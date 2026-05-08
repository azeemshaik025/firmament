//! Local Axum operator API for RFQ route shells and runtime projection reads.

pub mod auth;
pub mod runtime_read;
pub mod types;

use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::net::TcpListener;
use tracing::info;

use crate::application::rfq;
use crate::application::runtime::{
    AppState, RuntimeHandle, RuntimeOrchestrator, RuntimePersistence, RuntimeTrade,
};
use crate::config::{
    AppConfig, FIRMAMENT_ADMIN_PASSWORD_HASHES_ENV, FIRMAMENT_ADMIN_SESSION_SECRET_ENV,
};
use crate::domain::types::{MintAddress, QuoteId, SettlementStatus, TradeId};
use crate::error::{AppError, AppResult};
use crate::interfaces::http::auth::{
    parse_admin_password_hashes, sign_admin_session_cookie, validate_admin_session_cookie,
    verify_admin_password,
};
use crate::interfaces::http::types::{
    AdminLoginRequest, AdminMeResponse, AdminSummaryResponse, AssetResponse, ErrorBody,
    ErrorResponse, HtlcAcceptanceTerms, IntegrationStatus, LedgerSummary, QuoteAcceptResponse,
    RfqRequest, RfqResponse, RuntimeEventsResponse, RuntimeStateResponse, TakerLockRequest,
    TakerLockResponse, TakerRedeemRequest, TakerRedeemResponse, TradeAmounts, TradeResponse,
    WalletSettlementRequest, WalletSettlementResponse,
};

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

    /// Read the current trade shell.
    async fn trade(&self, trade_id: TradeId) -> Result<TradeResponse, ApiError>;

    /// Trigger an operator-requested rebalance check.
    async fn trigger_rebalance_check(&self) -> Result<(), ApiError>;

    /// Trigger an operator-requested Gateway refill check.
    async fn trigger_gateway_refill_check(&self) -> Result<(), ApiError>;
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
        .route("/v1/assets", get(get_assets))
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

        Ok(WalletSettlementResponse {
            quote_id: response.quote_id,
            trade_id: response.trade_id,
            taker_lock_transaction: response.taker_lock_transaction,
            expires_at: response.expires_at,
            integration_status: IntegrationStatus::RuntimeOrchestrated,
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

        Ok(TakerLockResponse {
            trade_id: response.trade_id,
            settlement_status: response.settlement_status,
            maker_lock_signature: response.maker_lock_signature,
            tx_signatures: response.tx_signatures,
            integration_status: IntegrationStatus::RuntimeOrchestrated,
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
            return Ok(TakerRedeemResponse {
                trade_id: prepared.trade_id,
                settlement_status: SettlementStatus::Initiated,
                taker_redeem_transaction: Some(prepared.taker_redeem_transaction),
                maker_redeem_signature: None,
                tx_signatures: Vec::new(),
                ledger_summary: ledger_summary()?,
                integration_status: IntegrationStatus::RuntimeOrchestrated,
            });
        };

        let completed = self
            .orchestrator
            .complete_wallet_taker_redeem(trade_id, request.preimage, signature)
            .await
            .map_err(ApiError::from_app_error)?;
        Ok(TakerRedeemResponse {
            trade_id: completed.trade.trade_id,
            settlement_status: completed.trade.settlement_status,
            taker_redeem_transaction: None,
            maker_redeem_signature: completed.maker_redeem_signature,
            tx_signatures: completed.trade.tx_signatures,
            ledger_summary: ledger_summary()?,
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

    async fn trigger_rebalance_check(&self) -> Result<(), ApiError> {
        self.orchestrator
            .trigger_operator_automation_check("rebalance")
            .await
            .map_err(ApiError::from_app_error)
    }

    async fn trigger_gateway_refill_check(&self) -> Result<(), ApiError> {
        self.orchestrator
            .trigger_operator_automation_check("gateway")
            .await
            .map_err(ApiError::from_app_error)
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

async fn get_assets(
    State(context): State<Arc<ApiContext>>,
) -> Result<Json<Vec<AssetResponse>>, ApiError> {
    Ok(Json(
        context
            .config
            .assets
            .supported
            .iter()
            .filter(|asset| asset.enabled)
            .map(|asset| AssetResponse {
                id: asset.id.to_string(),
                symbol: asset.symbol.clone(),
                mint: asset.mint.clone(),
                decimals: asset.decimals,
                min_trade_notional_usd: asset.min_trade_notional_usd,
                max_trade_notional_usd: asset.max_trade_notional_usd,
            })
            .collect(),
    ))
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
        return Err(ApiError::unauthorized());
    }

    let hashes = admin_password_hashes()?;
    if !verify_admin_password(username, &request.password, &hashes)
        .map_err(ApiError::from_app_error)?
    {
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
