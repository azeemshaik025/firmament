//! Public read endpoints: `/health`, `/v1/runtime/ledger`, `/v1/runtime/trades`.
//!
//! These handlers are unauthenticated, matching the posture of `/v1/runtime/state`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::adapters::persistence::ledger::LedgerAccountType;
use crate::application::runtime::RuntimeTrade;
use crate::domain::assets::AssetRegistry;
use crate::domain::types::{SettlementStatus, WalletAddress};

use super::types::{
    LedgerBalanceEntry, LedgerSnapshotResponse, TradeAmount, TradeSummary, TradesResponse,
};
use super::{ApiContext, ApiError};

/// Liveness check.
///
/// Returns `{"status":"ok","service":"firmament","version":"<crate-version>"}`.
/// Performs no DB or RPC dependency check — readiness probes belong to a
/// future endpoint variant.
pub async fn get_health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "firmament",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Query parameters for `GET /v1/runtime/ledger`.
#[derive(Debug, Default, Deserialize)]
pub struct LedgerQuery {
    /// Optional account type filter validated against
    /// [`LedgerAccountType::try_from`].
    pub account_type: Option<String>,
}

/// `GET /v1/runtime/ledger` and `GET /v1/runtime/ledger?account_type=<type>`.
///
/// Returns derived non-zero ledger balances. `entry_count` and `healthy`
/// always describe the whole ledger even when filtered. `healthy` is true
/// when the integrity report is healthy AND no protected account holds a
/// negative balance.
pub(crate) async fn get_ledger(
    State(context): State<Arc<ApiContext>>,
    Query(query): Query<LedgerQuery>,
) -> Result<Json<LedgerSnapshotResponse>, ApiError> {
    let persistence = context.persistence.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ledger_unavailable",
            "ledger persistence is not attached to this API router",
            Vec::new(),
        )
    })?;

    let filter = match query.account_type.as_deref() {
        None => None,
        Some(value) => Some(LedgerAccountType::try_from(value).map_err(|_| {
            ApiError::bad_request(
                "invalid_account_type",
                format!("unknown account_type '{value}'"),
            )
        })?),
    };

    let snapshot = persistence
        .ledger_snapshot()
        .map_err(ApiError::from_app_error)?;

    let registry = persistence.asset_registry();
    let balances: Vec<LedgerBalanceEntry> = snapshot
        .balances
        .into_iter()
        .filter(|balance| filter.is_none_or(|filter| balance.account.account_type == filter))
        .map(|balance| {
            let asset_id = balance.account.asset.clone();
            let decimals = registry
                .asset(&asset_id)
                .map_or(0, |metadata| metadata.decimals);
            let balance_raw = balance.balance_raw;
            let display_amount = format_signed_decimal(balance_raw, decimals);
            LedgerBalanceEntry {
                account_type: balance.account.account_type.to_string(),
                asset: asset_id.as_str().to_owned(),
                qualifier: balance.account.qualifier,
                balance_raw: balance_raw.to_string(),
                decimals,
                display_amount,
            }
        })
        .collect();

    Ok(Json(LedgerSnapshotResponse {
        healthy: snapshot.healthy,
        entry_count: snapshot.entry_count,
        balances,
    }))
}

/// Query parameters for `GET /v1/runtime/trades`.
#[derive(Debug, Default, Deserialize)]
pub struct TradesQuery {
    /// Number of trades to return. Default 10. Hard cap 100. Out-of-range
    /// or zero returns 400.
    pub limit: Option<u32>,
    /// Optional taker wallet filter for connected-wallet history drawers.
    pub wallet: Option<String>,
}

/// `GET /v1/runtime/trades?limit=<n>&wallet=<address>`.
///
/// Returns the most recent in-memory trades, newest first. Default limit
/// is 10 and the hard cap is 100. `total_count` and `successful_count`
/// describe the entire in-memory trade map; `successful_count` counts
/// trades whose settlement status is `redeemed`.
pub(crate) async fn get_trades(
    State(context): State<Arc<ApiContext>>,
    Query(query): Query<TradesQuery>,
) -> Result<Json<TradesResponse>, ApiError> {
    let limit = query.limit.unwrap_or(10);
    if limit == 0 || limit > 100 {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be 1..=100, got {limit}"),
        ));
    }
    let wallet = query
        .wallet
        .as_deref()
        .map(str::trim)
        .filter(|wallet| !wallet.is_empty())
        .map(WalletAddress::new);
    if query
        .wallet
        .as_deref()
        .is_some_and(|wallet| wallet.trim().is_empty())
    {
        return Err(ApiError::bad_request(
            "invalid_wallet",
            "wallet must not be empty",
        ));
    }

    let orchestrator = context.orchestrator.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "trades_unavailable",
            "runtime orchestrator is not attached to this API router",
            Vec::new(),
        )
    })?;

    let registry = orchestrator.asset_registry();
    let counts = if let Some(wallet) = wallet.as_ref() {
        orchestrator.trade_counts_for_wallet(wallet).await
    } else {
        orchestrator.trade_counts().await
    };
    let current_run = orchestrator.run_id();
    let recent = if let Some(wallet) = wallet.as_ref() {
        orchestrator
            .recent_trades_for_wallet(wallet, limit as usize)
            .await
    } else {
        orchestrator.recent_trades(limit as usize).await
    };
    let trades = recent
        .into_iter()
        .map(|trade| trade_to_summary(&trade, &registry, current_run))
        .collect();

    Ok(Json(TradesResponse {
        total_count: counts.total_count as u64,
        successful_count: counts.successful_count as u64,
        active_count: counts.active_count as u64,
        refunded_count: counts.refunded_count as u64,
        failed_count: counts.failed_count as u64,
        trades,
    }))
}

fn trade_to_summary(
    trade: &RuntimeTrade,
    registry: &AssetRegistry,
    current_run: crate::domain::types::RuntimeRunId,
) -> TradeSummary {
    TradeSummary {
        trade_id: trade.trade_id.to_string(),
        quote_id: trade.quote_id.to_string(),
        run_id: trade.run_id.to_string(),
        current_run: trade.run_id == current_run,
        taker_wallet: trade
            .taker_wallet
            .as_ref()
            .map(|wallet| wallet.as_str().to_owned()),
        expires_at: trade.expires_at.map(|expires_at| expires_at.to_string()),
        created_at: trade.created_at.to_string(),
        settlement_status: settlement_status_str(trade.settlement_status).to_owned(),
        input: amount_to_summary(&trade.input_amount, registry),
        output: amount_to_summary(&trade.output_amount, registry),
        tx_signatures: trade.tx_signature_kinds.clone(),
    }
}

fn amount_to_summary(
    amount: &crate::domain::types::TokenAmount,
    registry: &AssetRegistry,
) -> TradeAmount {
    let decimals = registry
        .asset(&amount.asset)
        .map_or(0, |metadata| metadata.decimals);
    let raw = i128::from(amount.amount_raw.as_u64());
    TradeAmount {
        asset: amount.asset.as_str().to_owned(),
        amount_raw: raw.to_string(),
        decimals,
        display_amount: format_signed_decimal(raw, decimals),
    }
}

fn settlement_status_str(status: SettlementStatus) -> &'static str {
    match status {
        SettlementStatus::Pending => "pending",
        SettlementStatus::Initiated => "initiated",
        SettlementStatus::Redeemed => "redeemed",
        SettlementStatus::Refunded => "refunded",
        SettlementStatus::Failed => "failed",
    }
}

/// Format a signed `i128` raw amount with `decimals` fractional digits using
/// fixed-width notation. The sign is preserved on negative balances so the
/// display reflects ledger drift directly.
pub(crate) fn format_signed_decimal(amount_raw: i128, decimals: u8) -> String {
    let negative = amount_raw < 0;
    let mut absolute = amount_raw.unsigned_abs();

    if decimals == 0 {
        let mut display = absolute.to_string();
        if negative {
            display.insert(0, '-');
        }
        return display;
    }

    let mut fractional_digits = String::with_capacity(usize::from(decimals));
    for _ in 0..decimals {
        let digit = u8::try_from(absolute % 10).unwrap_or(0);
        fractional_digits.insert(0, char::from(b'0' + digit));
        absolute /= 10;
    }
    let integer_digits = if absolute == 0 {
        "0".to_owned()
    } else {
        absolute.to_string()
    };

    let mut display = String::with_capacity(integer_digits.len() + 1 + fractional_digits.len() + 1);
    if negative {
        display.push('-');
    }
    display.push_str(&integer_digits);
    display.push('.');
    display.push_str(&fractional_digits);
    display
}
