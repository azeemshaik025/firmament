//! Always-on rebalance worker loop.
//!
//! Calls [`crate::application::runtime::RuntimeOrchestrator::run_rebalance_check`]
//! on the configured cadence and emits an
//! [`crate::domain::events::AutomationEvent::Tick`] (kind = `Rebalance`)
//! per tick. Errors are suppressed into the tick outcome — the loop
//! continues running so a transient adapter failure does not halt the
//! worker.

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
        let result = orchestrator.run_rebalance_check().await;
        let outcome = outcome_from_result(&result);
        if let Err(error) = &result {
            tracing::error!(
                target: "automation",
                kind = "rebalance",
                %error,
                "rebalance worker tick failed",
            );
        }
        publish_tick(orchestrator.as_ref(), AutomationKind::Rebalance, outcome).await;

        tokio::select! {
            () = shutdown.notified() => break,
            () = sleep(interval) => {}
        }
    }
}
