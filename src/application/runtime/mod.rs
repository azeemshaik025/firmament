//! Bootstrap state, read projection, and runtime orchestration glue.

mod bootstrap;
mod orchestrator;
mod projection;
pub mod reconciliation;

pub use bootstrap::{
    AppState, LiveRuntime, bootstrap, bootstrap_demo_runtime, bootstrap_live_runtime,
    bootstrap_maker_runtime,
};
pub use orchestrator::{
    AutomationRunSummary, LedgerReadSnapshot, RuntimeAdapters, RuntimeLedgerSummary,
    RuntimeOrchestrator, RuntimeOrchestratorOptions, RuntimePersistence, RuntimeTrade,
    TradeSignature, TradeSignatureKind, WalletSettlementStart, WalletTakerLockResult,
    WalletTakerRedeemPreparation, WalletTakerRedeemResult,
};
pub use projection::{
    GatewayProjection, InventoryProjection, PnlProjection, RebalanceProjection, RfqProjection,
    RiskProjection, RuntimeHandle, RuntimeState,
};
