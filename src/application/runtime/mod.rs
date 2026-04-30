//! Bootstrap state, read projection, and runtime orchestration glue.

mod bootstrap;
mod orchestrator;
mod projection;

pub use bootstrap::{AppState, LiveRuntime, bootstrap, bootstrap_live_runtime};
pub use orchestrator::{
    AutomationRunSummary, RuntimeAdapters, RuntimeLedgerSummary, RuntimeOrchestrator,
    RuntimeOrchestratorOptions, RuntimePersistence, RuntimeTrade,
};
pub use projection::{
    GatewayProjection, InventoryProjection, PnlProjection, RebalanceProjection, RfqProjection,
    RiskProjection, RuntimeHandle, RuntimeState,
};
