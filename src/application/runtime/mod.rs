//! Bootstrap state, read projection, and runtime orchestration glue.

pub mod automation;
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
    RuntimeTradeCounts, TradeSignature, TradeSignatureKind, WalletSettlementResume,
    WalletSettlementStart, WalletTakerLockResult, WalletTakerRedeemPreparation,
    WalletTakerRedeemResult, WalletTakerRefundPreparation, WalletTakerRefundResult,
};
pub use projection::{
    GatewayProjection, InventoryProjection, PnlProjection, RebalanceProjection, RfqProjection,
    RiskProjection, RuntimeHandle, RuntimeState,
};
