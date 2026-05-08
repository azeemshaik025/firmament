//! Public read endpoints: `/health`, `/v1/runtime/ledger`, `/v1/runtime/trades`.
//!
//! These handlers are unauthenticated, matching the posture of `/v1/runtime/state`.

use axum::Json;
use serde_json::{Value, json};

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
