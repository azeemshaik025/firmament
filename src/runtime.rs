//! Bootstrap state and read projection for Wave 0.

use std::sync::Arc;

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::RwLock;

use crate::config::AppConfig;
use crate::error::{AppError, AppResult};
use crate::events::{EventMetadata, RuntimeEvent, SystemEvent};
use crate::ports::RuntimeEventSink;
use crate::types::{AmountRaw, RejectionReason, RiskDecision, RuntimeRunId, TokenAmount};

/// Application state shared by future API, TUI, workers, and tests.
#[derive(Debug, Clone)]
pub struct AppState {
    config: AppConfig,
    runtime: RuntimeHandle,
    run_id: RuntimeRunId,
}

impl AppState {
    /// Borrow the loaded application config.
    #[must_use]
    pub const fn config(&self) -> &AppConfig {
        &self.config
    }

    /// Return a handle to the current runtime projection.
    #[must_use]
    pub fn runtime(&self) -> RuntimeHandle {
        self.runtime.clone()
    }

    /// Clone the latest runtime projection.
    pub async fn runtime_snapshot(&self) -> RuntimeState {
        self.runtime.snapshot().await
    }

    /// Return this process run identifier.
    #[must_use]
    pub const fn run_id(&self) -> RuntimeRunId {
        self.run_id
    }
}

/// Build the scaffold state without starting protocol workers.
///
/// # Errors
///
/// Returns an error when supplied config is invalid for the Wave 0 scaffold.
pub async fn bootstrap(config: AppConfig) -> AppResult<AppState> {
    config.validate()?;

    let run_id = RuntimeRunId::generate();
    let started_at = OffsetDateTime::now_utc();
    let ready_event = RuntimeEvent::System(SystemEvent::ScaffoldReady {
        metadata: EventMetadata::new(run_id),
        message: "runtime scaffold initialized; protocol workers are disabled".to_owned(),
    });

    let event_capacity = config.runtime.event_capacity;
    let runtime = RuntimeState::scaffold(run_id, started_at, &config, ready_event)?;
    let runtime = RuntimeHandle::new(runtime, event_capacity);

    Ok(AppState {
        config,
        runtime,
        run_id,
    })
}

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

    /// Clone the latest runtime projection for read-only API/TUI consumers.
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
    /// This in-memory scaffold is currently infallible, but the result shape
    /// matches the event sink trait used by future persistence-backed buffers.
    pub async fn publish_event(&self, event: RuntimeEvent) -> AppResult<()> {
        let mut state = self.inner.write().await;
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
}

#[async_trait]
impl RuntimeEventSink for RuntimeHandle {
    async fn publish(&self, event: RuntimeEvent) -> Result<(), AppError> {
        self.publish_event(event).await
    }
}

/// Runtime read projection consumed by future API and TUI layers.
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
    /// Rebalance and hedge placeholder projection.
    pub rebalance: RebalanceProjection,
    /// Gateway placeholder projection.
    pub gateway: GatewayProjection,
    /// Recent runtime events for API/TUI consumers.
    pub recent_events: Vec<RuntimeEvent>,
}

impl RuntimeState {
    fn scaffold(
        run_id: RuntimeRunId,
        started_at: OffsetDateTime,
        config: &AppConfig,
        ready_event: RuntimeEvent,
    ) -> AppResult<Self> {
        let mut recent_events = Vec::with_capacity(config.runtime.event_capacity.min(8));
        recent_events.push(ready_event);

        if recent_events.len() > config.runtime.event_capacity {
            return Err(AppError::internal(
                "scaffold event buffer exceeded configured capacity",
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
            status: "scaffold_ready".to_owned(),
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
    /// Hedge cost estimate in USDC.
    pub hedge_cost_usdc_estimate: Decimal,
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
            hedge_cost_usdc_estimate: Decimal::ZERO,
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

/// Rebalance and hedge placeholder projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebalanceProjection {
    /// Pending Jupiter swap count.
    pub pending_swap_count: usize,
    /// Completed Jupiter swap count.
    pub completed_swap_count: usize,
    /// Pending hedge action count.
    pub pending_hedge_count: usize,
    /// Completed hedge action count.
    pub completed_hedge_count: usize,
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
