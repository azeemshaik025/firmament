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

use super::types::{LedgerBalanceEntry, LedgerSnapshotResponse};
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

/// Format a signed `i128` raw amount with `decimals` fractional digits using
/// fixed-width notation. The sign is preserved on negative balances so the
/// display reflects ledger drift directly.
pub(crate) fn format_signed_decimal(amount_raw: i128, decimals: u8) -> String {
    let negative = amount_raw < 0;
    let absolute_i128 = if negative {
        amount_raw.unsigned_abs()
    } else {
        amount_raw as u128
    };
    let mut absolute = absolute_i128;

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
