//! Always-on automation workers (rebalance, Gateway refill, native SOL top-up).
//!
//! Three independent loops drive the scoped rebalance, Gateway refill, and
//! native SOL top-up checks at configurable cadences. Each loop is serialized
//! through the orchestrator's `automation_lock` so concurrent firings
//! (including the trade-driven `accept_quote` trigger) cannot stack overlapping
//! adapter actions or double-spend the cumulative cap. Each tick emits a
//! [`crate::domain::events::AutomationEvent::Tick`] with the resulting outcome
//! for operator visibility.
//!
//! Default-off in config; production opts in via `[runtime.automation]`.

mod excess_deposit_loop;
mod gateway_refill_loop;
mod native_top_up_loop;
mod rebalance_loop;

use std::sync::Arc;

use tokio::sync::Notify;

use crate::application::runtime::orchestrator::{AutomationRunSummary, RuntimeOrchestrator};
use crate::config::AutomationConfig;
use crate::domain::events::{
    AutomationEvent, AutomationKind, AutomationOutcome, EventMetadata, RuntimeEvent,
};
use crate::error::AppResult;

/// Spawn the three always-on automation loops. Each loop is detached and
/// honours `shutdown.notified()` between ticks.
pub fn spawn_workers(
    orchestrator: Arc<RuntimeOrchestrator>,
    config: &AutomationConfig,
    shutdown: Arc<Notify>,
) {
    tokio::spawn(rebalance_loop::run_loop(
        Arc::clone(&orchestrator),
        config.rebalance_interval_seconds,
        Arc::clone(&shutdown),
    ));
    tokio::spawn(gateway_refill_loop::run_loop(
        Arc::clone(&orchestrator),
        config.gateway_refill_interval_seconds,
        Arc::clone(&shutdown),
    ));
    tokio::spawn(native_top_up_loop::run_loop(
        Arc::clone(&orchestrator),
        config.native_top_up_interval_seconds,
        Arc::clone(&shutdown),
    ));
    tokio::spawn(excess_deposit_loop::run_loop(
        orchestrator,
        config.excess_deposit_interval_seconds,
        shutdown,
    ));
}

/// Convert one automation pass result into the public [`AutomationOutcome`]
/// projection used by [`AutomationEvent::Tick`].
pub(crate) fn outcome_from_result(result: &AppResult<AutomationRunSummary>) -> AutomationOutcome {
    match result {
        Ok(summary) => {
            if summary.completed_swaps == 0
                && summary.completed_gateway_refills == 0
                && summary.completed_gateway_deposits == 0
            {
                AutomationOutcome::NoActionNeeded
            } else {
                AutomationOutcome::Submitted {
                    completed_swaps: summary.completed_swaps,
                    completed_gateway_refills: summary.completed_gateway_refills,
                    completed_gateway_deposits: summary.completed_gateway_deposits,
                }
            }
        }
        Err(error) => AutomationOutcome::Failed {
            reason: error.to_string(),
        },
    }
}

/// Publish one `AutomationEvent::Tick` event for the supplied worker kind.
pub(crate) async fn publish_tick(
    orchestrator: &RuntimeOrchestrator,
    kind: AutomationKind,
    outcome: AutomationOutcome,
) {
    let event = RuntimeEvent::Automation(AutomationEvent::Tick {
        metadata: EventMetadata::new(orchestrator.run_id()),
        kind,
        outcome,
    });
    if let Err(error) = orchestrator.publish_runtime_event(event).await {
        tracing::error!(
            target: "automation",
            ?kind,
            %error,
            "automation tick event publish failed",
        );
    }
}
