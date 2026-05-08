//! Always-on Gateway refill worker loop.
//!
//! Same shape as the rebalance loop but emits with kind =
//! [`crate::domain::events::AutomationKind::GatewayRefill`]. The loop calls
//! the orchestrator's scoped Gateway refill check; concurrent execution is
//! serialized through the shared `automation_lock` mutex.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::sleep;

use crate::application::runtime::orchestrator::RuntimeOrchestrator;
use crate::domain::events::AutomationKind;

use super::{outcome_from_result, publish_tick};

pub async fn run_loop(
    orchestrator: Arc<RuntimeOrchestrator>,
    interval_seconds: u64,
    shutdown: Arc<Notify>,
) {
    let interval = Duration::from_secs(interval_seconds.max(1));
    loop {
        let result = orchestrator.run_gateway_refill_check().await;
        let outcome = outcome_from_result(&result);
        if let Err(error) = &result {
            tracing::error!(
                target: "automation",
                kind = "gateway_refill",
                %error,
                "gateway refill worker tick failed",
            );
        }
        publish_tick(
            orchestrator.as_ref(),
            AutomationKind::GatewayRefill,
            outcome,
        )
        .await;

        tokio::select! {
            () = shutdown.notified() => break,
            () = sleep(interval) => {}
        }
    }
}
