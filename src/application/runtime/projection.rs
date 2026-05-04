//! Runtime read projection and recent event buffer.

use std::sync::Arc;

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::RwLock;

use crate::config::AppConfig;
use crate::domain::events::{
    GatewayEvent, InventoryEvent, QuoteEvent, RiskEvent, RuntimeEvent, SettlementEvent, SwapEvent,
    SystemEvent,
};
use crate::domain::types::{
    AmountRaw, RejectionReason, RiskDecision, RuntimeRunId, SettlementStatus, TokenAmount,
};
use crate::error::{AppError, AppResult};
use crate::ports::RuntimeEventSink;

/// Thread-safe handle for runtime state snapshots and the recent event buffer.
#[derive(Debug, Clone)]
pub struct RuntimeHandle {
    inner: Arc<RwLock<RuntimeState>>,
    event_capacity: usize,
}

impl RuntimeHandle {
    /// Build a runtime handle around an initialized projection.
    #[must_use]
    pub fn new(state: RuntimeState, event_capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(state)),
            event_capacity,
        }
    }

    /// Clone the latest runtime projection for read-only API and web app consumers.
    pub async fn snapshot(&self) -> RuntimeState {
        self.inner.read().await.clone()
    }

    /// Return recent events in chronological order, optionally limited to the
    /// newest `limit` events.
    pub async fn recent_events(&self, limit: Option<usize>) -> Vec<RuntimeEvent> {
        let state = self.inner.read().await;
        let events = &state.recent_events;
        let start = limit.map_or(0, |limit| events.len().saturating_sub(limit));
        events[start..].to_vec()
    }

    /// Append a runtime event and evict oldest events beyond configured
    /// capacity.
    ///
    /// # Errors
    ///
    /// This in-memory projection is currently infallible, but the result shape
    /// matches the event sink trait used by future persistence-backed buffers.
    pub async fn publish_event(&self, event: RuntimeEvent) -> AppResult<()> {
        let mut state = self.inner.write().await;
        state.apply_event(&event);
        state.recent_events.push(event);

        let overflow = state
            .recent_events
            .len()
            .saturating_sub(self.event_capacity);
        if overflow > 0 {
            state.recent_events.drain(0..overflow);
        }

        Ok(())
    }

    /// Replace the P&L projection from durable persistence.
    pub async fn update_pnl_projection(&self, pnl: PnlProjection) {
        self.inner.write().await.pnl = pnl;
    }
}

#[async_trait]
impl RuntimeEventSink for RuntimeHandle {
    async fn publish(&self, event: RuntimeEvent) -> Result<(), AppError> {
        self.publish_event(event).await
    }
}

/// Runtime read projection consumed by API and web app layers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeState {
    /// Runtime invocation identifier.
    pub run_id: RuntimeRunId,
    /// UTC startup timestamp.
    pub started_at: OffsetDateTime,
    /// Inventory placeholder projection.
    pub inventory: InventoryProjection,
    /// Risk placeholder projection.
    pub risk: RiskProjection,
    /// P&L placeholder projection.
    pub pnl: PnlProjection,
    /// RFQ placeholder projection.
    pub rfq: RfqProjection,
    /// Rebalance placeholder projection.
    pub rebalance: RebalanceProjection,
    /// Gateway placeholder projection.
    pub gateway: GatewayProjection,
    /// Recent runtime events for API and web app consumers.
    pub recent_events: Vec<RuntimeEvent>,
}

impl RuntimeState {
    pub(super) fn bootstrap(
        run_id: RuntimeRunId,
        started_at: OffsetDateTime,
        config: &AppConfig,
        ready_event: RuntimeEvent,
    ) -> AppResult<Self> {
        let mut recent_events = Vec::with_capacity(config.runtime.event_capacity.min(8));
        recent_events.push(ready_event);

        if recent_events.len() > config.runtime.event_capacity {
            return Err(AppError::internal(
                "bootstrap event buffer exceeded configured capacity",
            ));
        }

        Ok(Self {
            run_id,
            started_at,
            inventory: InventoryProjection::from_config(config),
            risk: RiskProjection::from_config(config),
            pnl: PnlProjection::default(),
            rfq: RfqProjection::default(),
            rebalance: RebalanceProjection::default(),
            gateway: GatewayProjection::from_config(config),
            recent_events,
        })
    }

    fn apply_event(&mut self, event: &RuntimeEvent) {
        match event {
            RuntimeEvent::Quote(event) => self.apply_quote_event(event),
            RuntimeEvent::Settlement(event) => self.apply_settlement_event(event),
            RuntimeEvent::Swap(event) => self.apply_swap_event(event),
            RuntimeEvent::Gateway(event) => self.apply_gateway_event(event),
            RuntimeEvent::Risk(event) => self.apply_risk_event(event),
            RuntimeEvent::Inventory(event) => self.apply_inventory_event(event),
            RuntimeEvent::System(event) => self.apply_system_event(event),
        }
    }

    fn apply_quote_event(&mut self, event: &QuoteEvent) {
        match event {
            QuoteEvent::Requested { .. } => {
                self.rfq.active_quote_count = self.rfq.active_quote_count.saturating_add(1);
            }
            QuoteEvent::Accepted { .. } => {
                self.rfq.accepted_quote_count = self.rfq.accepted_quote_count.saturating_add(1);
                self.rfq.active_quote_count = self.rfq.active_quote_count.saturating_sub(1);
            }
            QuoteEvent::Rejected {
                reason, decision, ..
            } => {
                self.rfq.rejected_quote_count = self.rfq.rejected_quote_count.saturating_add(1);
                self.rfq.active_quote_count = self.rfq.active_quote_count.saturating_sub(1);
                self.risk.last_decision = Some(decision.clone());
                push_unique_rejection(&mut self.risk.active_rejections, reason.clone());
            }
            QuoteEvent::Expired { .. } => {
                self.rfq.active_quote_count = self.rfq.active_quote_count.saturating_sub(1);
            }
        }
    }

    fn apply_settlement_event(&mut self, event: &SettlementEvent) {
        match event {
            SettlementEvent::Started { .. } => {
                self.rfq.active_settlement_count =
                    self.rfq.active_settlement_count.saturating_add(1);
            }
            SettlementEvent::StatusChanged { status, .. } => {
                if matches!(
                    status,
                    SettlementStatus::Redeemed
                        | SettlementStatus::Refunded
                        | SettlementStatus::Failed
                ) {
                    self.rfq.active_settlement_count =
                        self.rfq.active_settlement_count.saturating_sub(1);
                }
            }
            SettlementEvent::Refunded { .. } | SettlementEvent::Failed { .. } => {
                self.rfq.active_settlement_count =
                    self.rfq.active_settlement_count.saturating_sub(1);
            }
            SettlementEvent::Initiated { .. } | SettlementEvent::Redeemed { .. } => {}
        }
    }

    fn apply_swap_event(&mut self, event: &SwapEvent) {
        match event {
            SwapEvent::PriceObserved { .. } => {}
            SwapEvent::Quoted { .. } => {
                self.rebalance.pending_swap_count =
                    self.rebalance.pending_swap_count.saturating_add(1);
            }
            SwapEvent::Executed { .. } => {
                self.rebalance.pending_swap_count =
                    self.rebalance.pending_swap_count.saturating_sub(1);
                self.rebalance.completed_swap_count =
                    self.rebalance.completed_swap_count.saturating_add(1);
            }
            SwapEvent::Failed { .. } => {
                self.rebalance.pending_swap_count =
                    self.rebalance.pending_swap_count.saturating_sub(1);
            }
        }
    }

    fn apply_gateway_event(&mut self, event: &GatewayEvent) {
        let status = match event {
            GatewayEvent::BalanceChecked { .. } => "checked",
            GatewayEvent::RefillRequested { .. } => "refill_requested",
            GatewayEvent::RefillCompleted { .. } => "refill_completed",
            GatewayEvent::Failed { .. } => "failed",
        };
        status.clone_into(&mut self.gateway.status);
    }

    fn apply_risk_event(&mut self, event: &RiskEvent) {
        if let RiskEvent::Evaluated { decision, .. } = event {
            self.risk.last_decision = Some(decision.clone());
            if let RiskDecision::Rejected { reason, .. } = decision {
                push_unique_rejection(&mut self.risk.active_rejections, reason.clone());
            }
        }
    }

    fn apply_inventory_event(&mut self, event: &InventoryEvent) {
        match event {
            InventoryEvent::Snapshot { snapshot, .. } => {
                self.inventory.balances.clone_from(&snapshot.balances);
                "snapshot".clone_into(&mut self.inventory.status);
            }
            InventoryEvent::DriftDetected { drift_bps, .. } => {
                self.inventory.max_drift_bps = self
                    .inventory
                    .max_drift_bps
                    .saturating_abs()
                    .max(drift_bps.saturating_abs());
                "drift".clone_into(&mut self.inventory.status);
            }
            InventoryEvent::ThresholdBreached { reason, .. } => {
                "threshold".clone_into(&mut self.inventory.status);
                push_unique_rejection(&mut self.risk.active_rejections, reason.clone());
            }
        }
    }

    fn apply_system_event(&mut self, event: &SystemEvent) {
        match event {
            SystemEvent::HealthChanged {
                component, status, ..
            } => {
                if component == "inventory" {
                    self.inventory.status.clone_from(status);
                }
            }
            SystemEvent::RuntimeReady { .. }
            | SystemEvent::Shutdown { .. }
            | SystemEvent::TransactionObserved { .. } => {}
        }
    }
}

/// Inventory placeholder projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryProjection {
    /// Placeholder balances. Live balance readers are added later.
    pub balances: Vec<TokenAmount>,
    /// Configured quoteable thresholds for display.
    pub quoteable_thresholds: Vec<TokenAmount>,
    /// Current maximum drift in basis points.
    pub max_drift_bps: i32,
    /// Operator-facing status label.
    pub status: String,
}

impl InventoryProjection {
    fn from_config(config: &AppConfig) -> Self {
        let quoteable_thresholds = config
            .assets
            .supported
            .iter()
            .map(|asset| TokenAmount::new(asset.id.clone(), asset.quoteable_threshold_raw))
            .collect();

        Self {
            balances: Vec::new(),
            quoteable_thresholds,
            max_drift_bps: 0,
            status: "runtime_ready".to_owned(),
        }
    }
}

/// Risk placeholder projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskProjection {
    /// Most recent risk decision, if any.
    pub last_decision: Option<RiskDecision>,
    /// Active rejection reasons visible to the operator.
    pub active_rejections: Vec<RejectionReason>,
    /// Whether taker allowlist enforcement is configured.
    pub require_taker_allowlist: bool,
    /// Maximum allowed price staleness.
    pub max_price_staleness_seconds: u64,
}

impl RiskProjection {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            last_decision: None,
            active_rejections: Vec::new(),
            require_taker_allowlist: config.risk.require_taker_allowlist,
            max_price_staleness_seconds: config.risk.max_price_staleness_seconds,
        }
    }
}

/// P&L placeholder projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PnlProjection {
    /// Realized spread estimate in USDC.
    pub realized_spread_usdc_estimate: Decimal,
    /// Fee estimate in USDC.
    pub fees_usdc_estimate: Decimal,
    /// Rebalance cost estimate in USDC.
    pub rebalance_cost_usdc_estimate: Decimal,
    /// Net estimate in USDC.
    pub net_usdc_estimate: Decimal,
}

impl Default for PnlProjection {
    fn default() -> Self {
        Self {
            realized_spread_usdc_estimate: Decimal::ZERO,
            fees_usdc_estimate: Decimal::ZERO,
            rebalance_cost_usdc_estimate: Decimal::ZERO,
            net_usdc_estimate: Decimal::ZERO,
        }
    }
}

/// RFQ placeholder projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RfqProjection {
    /// Active quote count.
    pub active_quote_count: usize,
    /// Accepted quote count.
    pub accepted_quote_count: usize,
    /// Rejected quote count.
    pub rejected_quote_count: usize,
    /// Current settlement count.
    pub active_settlement_count: usize,
}

/// Rebalance placeholder projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebalanceProjection {
    /// Pending Jupiter swap count.
    pub pending_swap_count: usize,
    /// Completed Jupiter swap count.
    pub completed_swap_count: usize,
}

/// Gateway placeholder projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayProjection {
    /// Whether Gateway is configured as enabled.
    pub enabled: bool,
    /// Working wallet refill threshold.
    pub usdc_refill_threshold_raw: AmountRaw,
    /// Working wallet refill target.
    pub usdc_refill_target_raw: AmountRaw,
    /// Operator-facing status label.
    pub status: String,
}

impl GatewayProjection {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            enabled: config.gateway.enabled,
            usdc_refill_threshold_raw: config.gateway.usdc_refill_threshold_raw,
            usdc_refill_target_raw: config.gateway.usdc_refill_target_raw,
            status: "not_checked".to_owned(),
        }
    }
}

fn push_unique_rejection(rejections: &mut Vec<RejectionReason>, reason: RejectionReason) {
    if !rejections.contains(&reason) {
        rejections.push(reason);
    }
}
