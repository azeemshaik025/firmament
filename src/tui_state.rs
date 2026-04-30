//! Pure operator cockpit state and runtime-to-view projection.

use std::cmp;

use crate::{
    events::{
        GatewayEvent, InventoryEvent, QuoteEvent, RiskEvent, RuntimeEvent, SettlementEvent,
        SwapEvent, SystemEvent,
    },
    runtime::RuntimeState,
    types::{
        AssetPair, LedgerEntryCategory, RejectionReason, RiskDecision, SettlementStatus,
        TokenAmount,
    },
};

const TAB_COUNT: usize = 6;

/// Top-level cockpit tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorTab {
    /// Runtime health, inventory summary, and recent events.
    Overview,
    /// RFQ lifecycle and settlement activity.
    Rfqs,
    /// Working inventory, Gateway status, and thresholds.
    Liquidity,
    /// Risk decisions, active limits, and rejection reasons.
    Risk,
    /// P&L estimates and ledger-oriented movement summaries.
    LedgerPnl,
    /// Jupiter rebalance and exposure hedge activity.
    RebalanceHedge,
}

impl OperatorTab {
    /// Ordered tab list for rendering and navigation.
    pub const ALL: [Self; TAB_COUNT] = [
        Self::Overview,
        Self::Rfqs,
        Self::Liquidity,
        Self::Risk,
        Self::LedgerPnl,
        Self::RebalanceHedge,
    ];

    /// Human-readable tab title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Rfqs => "RFQs",
            Self::Liquidity => "Liquidity",
            Self::Risk => "Risk",
            Self::LedgerPnl => "Ledger/P&L",
            Self::RebalanceHedge => "Rebalance/Hedge",
        }
    }

    /// Zero-based tab index.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Rfqs => 1,
            Self::Liquidity => 2,
            Self::Risk => 3,
            Self::LedgerPnl => 4,
            Self::RebalanceHedge => 5,
        }
    }

    #[must_use]
    const fn from_index(index: usize) -> Self {
        match index {
            0 => Self::Overview,
            1 => Self::Rfqs,
            2 => Self::Liquidity,
            3 => Self::Risk,
            4 => Self::LedgerPnl,
            _ => Self::RebalanceHedge,
        }
    }

    #[must_use]
    const fn next(self) -> Self {
        Self::from_index((self.index() + 1) % TAB_COUNT)
    }

    #[must_use]
    const fn previous(self) -> Self {
        Self::from_index((self.index() + TAB_COUNT - 1) % TAB_COUNT)
    }
}

/// Keyboard-independent user intent consumed by the cockpit state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiInput {
    /// Move to the next top-level tab.
    NextTab,
    /// Move to the previous top-level tab.
    PreviousTab,
    /// Request graceful shutdown.
    Quit,
    /// Execute a scripted demo placeholder command.
    Demo(ScriptedDemoAction),
}

/// Scripted demo actions emitted by the TUI shell for later runtime wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptedDemoAction {
    /// Generate a normal tiny RFQ.
    GenerateTinyRfq,
    /// Generate an oversized RFQ to demonstrate rejection.
    GenerateOversizedRfq,
    /// Accept the selected quote.
    AcceptQuote,
    /// Trigger a rebalance check.
    TriggerRebalanceCheck,
    /// Trigger a Gateway refill check.
    TriggerGatewayRefillCheck,
    /// Trigger an exposure hedge check.
    TriggerExposureHedgeCheck,
}

impl ScriptedDemoAction {
    /// Stable command label for tracing and future command bus integration.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::GenerateTinyRfq => "generate_tiny_rfq",
            Self::GenerateOversizedRfq => "generate_oversized_rfq",
            Self::AcceptQuote => "accept_quote",
            Self::TriggerRebalanceCheck => "trigger_rebalance_check",
            Self::TriggerGatewayRefillCheck => "trigger_gateway_refill_check",
            Self::TriggerExposureHedgeCheck => "trigger_exposure_hedge_check",
        }
    }
}

/// Typed command returned by the TUI shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorCommand {
    /// Request graceful shutdown.
    Quit,
    /// Placeholder demo action to be handled by runtime orchestration later.
    Demo(ScriptedDemoAction),
}

/// Ephemeral TUI selection state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiState {
    selected_tab: OperatorTab,
    selected_rows: [usize; TAB_COUNT],
    should_quit: bool,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            selected_tab: OperatorTab::Overview,
            selected_rows: [0; TAB_COUNT],
            should_quit: false,
        }
    }
}

impl TuiState {
    /// Return the selected tab.
    #[must_use]
    pub const fn selected_tab(&self) -> OperatorTab {
        self.selected_tab
    }

    /// Whether graceful shutdown has been requested.
    #[must_use]
    pub const fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Return the selected row for `tab`, clamped to a known row count.
    #[must_use]
    pub fn selected_row_for(&self, tab: OperatorTab, row_count: usize) -> usize {
        if row_count == 0 {
            return 0;
        }

        cmp::min(self.selected_rows[tab.index()], row_count - 1)
    }

    /// Move the selected row down within the current tab.
    pub fn select_next_row(&mut self, row_count: usize) {
        if row_count == 0 {
            self.selected_rows[self.selected_tab.index()] = 0;
            return;
        }

        let current = self.selected_row_for(self.selected_tab, row_count);
        self.selected_rows[self.selected_tab.index()] = (current + 1) % row_count;
    }

    /// Move the selected row up within the current tab.
    pub fn select_previous_row(&mut self, row_count: usize) {
        if row_count == 0 {
            self.selected_rows[self.selected_tab.index()] = 0;
            return;
        }

        let current = self.selected_row_for(self.selected_tab, row_count);
        self.selected_rows[self.selected_tab.index()] = (current + row_count - 1) % row_count;
    }

    /// Apply a keyboard-independent input and return any command it emits.
    #[must_use]
    pub fn apply_input(&mut self, input: TuiInput) -> Option<OperatorCommand> {
        match input {
            TuiInput::NextTab => {
                self.selected_tab = self.selected_tab.next();
                None
            }
            TuiInput::PreviousTab => {
                self.selected_tab = self.selected_tab.previous();
                None
            }
            TuiInput::Quit => {
                self.should_quit = true;
                Some(OperatorCommand::Quit)
            }
            TuiInput::Demo(action) => Some(OperatorCommand::Demo(action)),
        }
    }
}

/// One status row projected for a cockpit table or panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRow {
    /// Human-readable row label.
    pub label: String,
    /// Human-readable value.
    pub value: String,
    /// Short status label.
    pub status: String,
}

impl StatusRow {
    /// Build a status row from displayable parts.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        value: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            status: status.into(),
        }
    }
}

/// Runtime projection shaped for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiViewModel {
    /// Overview rows.
    pub overview_rows: Vec<StatusRow>,
    /// RFQ rows.
    pub rfq_rows: Vec<StatusRow>,
    /// Liquidity rows.
    pub liquidity_rows: Vec<StatusRow>,
    /// Risk rows.
    pub risk_rows: Vec<StatusRow>,
    /// Ledger and P&L rows.
    pub ledger_rows: Vec<StatusRow>,
    /// Rebalance and hedge rows.
    pub rebalance_rows: Vec<StatusRow>,
    /// Recent event rows.
    pub event_rows: Vec<StatusRow>,
}

impl TuiViewModel {
    /// Project runtime state into a renderable view model.
    #[must_use]
    pub fn from_runtime(runtime: &RuntimeState) -> Self {
        Self {
            overview_rows: overview_rows(runtime),
            rfq_rows: rfq_rows(runtime),
            liquidity_rows: liquidity_rows(runtime),
            risk_rows: risk_rows(runtime),
            ledger_rows: ledger_rows(runtime),
            rebalance_rows: rebalance_rows(runtime),
            event_rows: event_rows(runtime),
        }
    }

    /// Borrow rows for a selected tab.
    #[must_use]
    pub fn rows_for_tab(&self, tab: OperatorTab) -> &[StatusRow] {
        match tab {
            OperatorTab::Overview => &self.overview_rows,
            OperatorTab::Rfqs => &self.rfq_rows,
            OperatorTab::Liquidity => &self.liquidity_rows,
            OperatorTab::Risk => &self.risk_rows,
            OperatorTab::LedgerPnl => &self.ledger_rows,
            OperatorTab::RebalanceHedge => &self.rebalance_rows,
        }
    }
}

fn overview_rows(runtime: &RuntimeState) -> Vec<StatusRow> {
    vec![
        StatusRow::new("Run", runtime.run_id.to_string(), "ready"),
        StatusRow::new(
            "Started",
            runtime
                .started_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| runtime.started_at.unix_timestamp().to_string()),
            "utc",
        ),
        StatusRow::new(
            "Inventory",
            runtime.inventory.status.as_str(),
            inventory_status(&runtime.inventory.status),
        ),
        StatusRow::new(
            "Gateway",
            runtime.gateway.status.as_str(),
            if runtime.gateway.enabled {
                "enabled"
            } else {
                "disabled"
            },
        ),
        StatusRow::new(
            "Recent events",
            runtime.recent_events.len().to_string(),
            if runtime.recent_events.is_empty() {
                "quiet"
            } else {
                "active"
            },
        ),
    ]
}

fn rfq_rows(runtime: &RuntimeState) -> Vec<StatusRow> {
    vec![
        count_row(
            "Active quotes",
            runtime.rfq.active_quote_count,
            "active",
            "idle",
        ),
        count_row(
            "Accepted quotes",
            runtime.rfq.accepted_quote_count,
            "accepted",
            "none",
        ),
        count_row(
            "Rejected quotes",
            runtime.rfq.rejected_quote_count,
            "rejected",
            "clear",
        ),
        count_row(
            "Settlements",
            runtime.rfq.active_settlement_count,
            "settling",
            "idle",
        ),
    ]
}

fn liquidity_rows(runtime: &RuntimeState) -> Vec<StatusRow> {
    let mut rows = Vec::new();

    if runtime.inventory.balances.is_empty() {
        rows.push(StatusRow::new("Working balances", "none observed", "empty"));
    } else {
        rows.extend(
            runtime
                .inventory
                .balances
                .iter()
                .map(|amount| StatusRow::new("Working balance", format_amount(amount), "live")),
        );
    }

    rows.extend(runtime.inventory.quoteable_thresholds.iter().map(|amount| {
        StatusRow::new(
            format!("{} threshold", amount.asset),
            amount.amount_raw.as_u64().to_string(),
            "quoteable",
        )
    }));

    rows.push(StatusRow::new(
        "Max drift",
        format!("{} bps", runtime.inventory.max_drift_bps),
        if runtime.inventory.max_drift_bps == 0 {
            "balanced"
        } else {
            "drift"
        },
    ));
    rows.push(StatusRow::new(
        "Gateway refill threshold",
        runtime
            .gateway
            .usdc_refill_threshold_raw
            .as_u64()
            .to_string(),
        "usdc",
    ));
    rows.push(StatusRow::new(
        "Gateway refill target",
        runtime.gateway.usdc_refill_target_raw.as_u64().to_string(),
        "usdc",
    ));

    rows
}

fn risk_rows(runtime: &RuntimeState) -> Vec<StatusRow> {
    let mut rows = vec![
        StatusRow::new(
            "Taker allowlist",
            if runtime.risk.require_taker_allowlist {
                "required"
            } else {
                "open"
            },
            if runtime.risk.require_taker_allowlist {
                "required"
            } else {
                "off"
            },
        ),
        StatusRow::new(
            "Price staleness",
            format!("{}s", runtime.risk.max_price_staleness_seconds),
            "limit",
        ),
    ];

    rows.push(match &runtime.risk.last_decision {
        Some(RiskDecision::Accepted { checks }) => StatusRow::new(
            "Last decision",
            format!("accepted: {}", join_or_none(checks)),
            "accepted",
        ),
        Some(RiskDecision::Rejected { reason, details }) => StatusRow::new(
            "Last decision",
            format!(
                "{}: {}",
                rejection_reason_label(reason),
                join_or_none(details)
            ),
            "rejected",
        ),
        None => StatusRow::new("Last decision", "not evaluated", "idle"),
    });

    if runtime.risk.active_rejections.is_empty() {
        rows.push(StatusRow::new("Active rejection", "none", "clear"));
    } else {
        rows.extend(runtime.risk.active_rejections.iter().map(|reason| {
            StatusRow::new(
                "Active rejection",
                rejection_reason_label(reason),
                "rejected",
            )
        }));
    }

    rows
}

fn ledger_rows(runtime: &RuntimeState) -> Vec<StatusRow> {
    vec![
        decimal_row(
            LedgerEntryCategory::ProfitAndLoss,
            "Realized spread",
            runtime.pnl.realized_spread_usdc_estimate,
        ),
        decimal_row(
            LedgerEntryCategory::Fees,
            "Fees",
            runtime.pnl.fees_usdc_estimate,
        ),
        decimal_row(
            LedgerEntryCategory::Hedge,
            "Hedge cost",
            runtime.pnl.hedge_cost_usdc_estimate,
        ),
        decimal_row(
            LedgerEntryCategory::Rebalance,
            "Rebalance cost",
            runtime.pnl.rebalance_cost_usdc_estimate,
        ),
        StatusRow::new(
            "Net estimate",
            format!("{} USDC", runtime.pnl.net_usdc_estimate),
            if runtime.pnl.net_usdc_estimate.is_sign_negative() {
                "negative"
            } else {
                "flat"
            },
        ),
    ]
}

fn rebalance_rows(runtime: &RuntimeState) -> Vec<StatusRow> {
    vec![
        count_row(
            "Pending swaps",
            runtime.rebalance.pending_swap_count,
            "pending",
            "idle",
        ),
        count_row(
            "Completed swaps",
            runtime.rebalance.completed_swap_count,
            "completed",
            "none",
        ),
        count_row(
            "Pending hedges",
            runtime.rebalance.pending_hedge_count,
            "pending",
            "idle",
        ),
        count_row(
            "Completed hedges",
            runtime.rebalance.completed_hedge_count,
            "completed",
            "none",
        ),
    ]
}

fn event_rows(runtime: &RuntimeState) -> Vec<StatusRow> {
    runtime
        .recent_events
        .iter()
        .rev()
        .take(12)
        .map(project_event)
        .collect()
}

fn project_event(event: &RuntimeEvent) -> StatusRow {
    match event {
        RuntimeEvent::Quote(event) => project_quote_event(event),
        RuntimeEvent::Settlement(event) => project_settlement_event(event),
        RuntimeEvent::Swap(event) => project_swap_event(event),
        RuntimeEvent::Gateway(event) => project_gateway_event(event),
        RuntimeEvent::Risk(event) => project_risk_event(event),
        RuntimeEvent::Inventory(event) => project_inventory_event(event),
        RuntimeEvent::System(event) => project_system_event(event),
    }
}

fn project_quote_event(event: &QuoteEvent) -> StatusRow {
    match event {
        QuoteEvent::Requested {
            quote_id,
            pair,
            input_amount,
            ..
        } => StatusRow::new(
            "RFQ",
            format!(
                "{quote_id} {} input {}",
                format_pair(pair),
                format_amount(input_amount)
            ),
            "requested",
        ),
        QuoteEvent::Accepted {
            quote_id, trade_id, ..
        } => StatusRow::new(
            "RFQ",
            format!("{quote_id} accepted as {trade_id}"),
            "accepted",
        ),
        QuoteEvent::Rejected { reason, .. } => {
            StatusRow::new("RFQ", rejection_reason_label(reason), "rejected")
        }
        QuoteEvent::Expired { quote_id, .. } => {
            StatusRow::new("RFQ", quote_id.to_string(), "expired")
        }
    }
}

fn project_settlement_event(event: &SettlementEvent) -> StatusRow {
    match event {
        SettlementEvent::Started {
            trade_id, quote_id, ..
        } => StatusRow::new(
            "Settlement",
            format!("{trade_id} from quote {quote_id}"),
            "started",
        ),
        SettlementEvent::Initiated { receipt, .. } => {
            settlement_receipt_row("initiated", receipt.trade_id, receipt.signature.as_ref())
        }
        SettlementEvent::Redeemed { receipt, .. } => {
            settlement_receipt_row("redeemed", receipt.trade_id, receipt.signature.as_ref())
        }
        SettlementEvent::Refunded { receipt, .. } => {
            settlement_receipt_row("refunded", receipt.trade_id, receipt.signature.as_ref())
        }
        SettlementEvent::StatusChanged {
            trade_id, status, ..
        } => StatusRow::new(
            "Settlement",
            format!("{trade_id} {}", settlement_status_label(*status)),
            settlement_status_label(*status),
        ),
        SettlementEvent::Failed {
            trade_id, reason, ..
        } => StatusRow::new("Settlement", format!("{trade_id}: {reason}"), "failed"),
    }
}

fn project_swap_event(event: &SwapEvent) -> StatusRow {
    match event {
        SwapEvent::PriceObserved { price, .. } => StatusRow::new(
            "Rebalance",
            format!(
                "{} price {}",
                format_pair(&price.pair),
                price.output_per_input
            ),
            "price",
        ),
        SwapEvent::Quoted { quote, .. } => StatusRow::new(
            "Rebalance",
            format!(
                "{} expected {}",
                format_pair(&quote.request.pair),
                format_amount(&quote.expected_output)
            ),
            "quoted",
        ),
        SwapEvent::Executed { receipt, .. } => StatusRow::new(
            "Rebalance",
            format!("swap {}", receipt.signature),
            "executed",
        ),
        SwapEvent::Failed { pair, reason, .. } => StatusRow::new(
            "Rebalance",
            format!(
                "{}: {reason}",
                pair.as_ref().map_or("unknown pair".to_owned(), format_pair)
            ),
            "failed",
        ),
    }
}

fn project_gateway_event(event: &GatewayEvent) -> StatusRow {
    match event {
        GatewayEvent::BalanceChecked { balance, .. } => {
            StatusRow::new("Gateway", format_amount(balance), "checked")
        }
        GatewayEvent::RefillRequested { amount, .. } => {
            StatusRow::new("Gateway", format_amount(amount), "refill_requested")
        }
        GatewayEvent::RefillCompleted { receipt, .. } => StatusRow::new(
            "Gateway",
            format!("completed {}", format_amount(&receipt.amount)),
            "refill_completed",
        ),
        GatewayEvent::Failed { reason, .. } => StatusRow::new("Gateway", reason, "failed"),
    }
}

fn project_risk_event(event: &RiskEvent) -> StatusRow {
    match event {
        RiskEvent::Evaluated { decision, .. } => match decision {
            RiskDecision::Accepted { checks } => {
                StatusRow::new("Risk", join_or_none(checks), "accepted")
            }
            RiskDecision::Rejected { reason, details } => StatusRow::new(
                "Risk",
                format!(
                    "{}: {}",
                    rejection_reason_label(reason),
                    join_or_none(details)
                ),
                "rejected",
            ),
        },
        RiskEvent::LimitUpdated { limit, value, .. } => {
            StatusRow::new("Risk", format!("{limit}={value}"), "limit_updated")
        }
    }
}

fn project_inventory_event(event: &InventoryEvent) -> StatusRow {
    match event {
        InventoryEvent::Snapshot { snapshot, .. } => StatusRow::new(
            "Liquidity",
            format!("{} balances", snapshot.balances.len()),
            "snapshot",
        ),
        InventoryEvent::DriftDetected {
            asset, drift_bps, ..
        } => StatusRow::new(
            "Liquidity",
            format!("{} drift {drift_bps} bps", format_amount(asset)),
            "drift",
        ),
        InventoryEvent::ThresholdBreached { asset, reason, .. } => StatusRow::new(
            "Liquidity",
            format!(
                "{} {}",
                format_amount(asset),
                rejection_reason_label(reason)
            ),
            "threshold",
        ),
    }
}

fn project_system_event(event: &SystemEvent) -> StatusRow {
    match event {
        SystemEvent::ScaffoldReady { message, .. } => StatusRow::new("System", message, "ready"),
        SystemEvent::HealthChanged {
            component, status, ..
        } => StatusRow::new("System", format!("{component}: {status}"), status),
        SystemEvent::Shutdown { reason, .. } => StatusRow::new("System", reason, "shutdown"),
        SystemEvent::TransactionObserved {
            trade_id,
            signature,
            ..
        } => StatusRow::new(
            "System",
            format!(
                "{} {signature}",
                trade_id.map_or("untracked trade".to_owned(), |id| id.to_string())
            ),
            "tx",
        ),
    }
}

fn count_row(
    label: &'static str,
    count: usize,
    active_status: &'static str,
    empty_status: &'static str,
) -> StatusRow {
    StatusRow::new(
        label,
        count.to_string(),
        if count == 0 {
            empty_status
        } else {
            active_status
        },
    )
}

fn decimal_row(
    category: LedgerEntryCategory,
    label: &'static str,
    amount: rust_decimal::Decimal,
) -> StatusRow {
    StatusRow::new(
        label,
        format!("{amount} USDC"),
        ledger_category_label(category),
    )
}

fn settlement_receipt_row(
    status: &'static str,
    trade_id: crate::types::TradeId,
    signature: Option<&crate::types::TxSignature>,
) -> StatusRow {
    StatusRow::new(
        "Settlement",
        signature.map_or_else(
            || trade_id.to_string(),
            |signature| format!("{trade_id} {signature}"),
        ),
        status,
    )
}

fn format_pair(pair: &AssetPair) -> String {
    format!("{}->{}", pair.input, pair.output)
}

fn format_amount(amount: &TokenAmount) -> String {
    format!("{} {}", amount.amount_raw.as_u64(), amount.asset)
}

fn join_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

fn inventory_status(status: &str) -> &'static str {
    match status {
        "scaffold_ready" | "ready" => "ready",
        "empty" => "empty",
        _ => "status",
    }
}

fn settlement_status_label(status: SettlementStatus) -> &'static str {
    match status {
        SettlementStatus::Pending => "pending",
        SettlementStatus::Initiated => "initiated",
        SettlementStatus::Redeemed => "redeemed",
        SettlementStatus::Refunded => "refunded",
        SettlementStatus::Failed => "failed",
    }
}

fn ledger_category_label(category: LedgerEntryCategory) -> &'static str {
    match category {
        LedgerEntryCategory::Quote => "quote",
        LedgerEntryCategory::Fill => "fill",
        LedgerEntryCategory::HtlcEscrow => "escrow",
        LedgerEntryCategory::Fees => "fees",
        LedgerEntryCategory::Rebalance => "rebalance",
        LedgerEntryCategory::Hedge => "hedge",
        LedgerEntryCategory::ProfitAndLoss => "pnl",
    }
}

fn rejection_reason_label(reason: &RejectionReason) -> &'static str {
    match reason {
        RejectionReason::UnsupportedPair => "unsupported pair",
        RejectionReason::UnsupportedAsset => "unsupported asset",
        RejectionReason::AmountTooSmall => "amount too small",
        RejectionReason::MaxNotionalExceeded => "max notional exceeded",
        RejectionReason::CumulativeCapExceeded => "cumulative cap exceeded",
        RejectionReason::InventoryBelowQuoteableThreshold => "inventory below quoteable threshold",
        RejectionReason::ExposureLimitExceeded => "exposure limit exceeded",
        RejectionReason::StalePrice => "stale price",
        RejectionReason::WalletNotAllowed => "wallet not allowed",
        RejectionReason::GatewayUnavailable => "gateway unavailable",
        RejectionReason::ExternalServiceUnavailable => "external service unavailable",
        RejectionReason::ValidationFailed => "validation failed",
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use time::OffsetDateTime;

    use super::*;
    use crate::{
        events::{EventMetadata, QuoteEvent, RuntimeEvent},
        runtime::{
            GatewayProjection, InventoryProjection, PnlProjection, RebalanceProjection,
            RfqProjection, RiskProjection,
        },
        types::{
            AmountRaw, AssetId, AssetPair, RejectionReason, RiskDecision, RuntimeRunId,
            TokenAmount, WalletAddress,
        },
    };

    #[test]
    fn tui_state_tab_navigation_wraps() {
        let mut state = TuiState::default();

        assert_eq!(state.selected_tab(), OperatorTab::Overview);
        assert_eq!(state.apply_input(TuiInput::PreviousTab), None);
        assert_eq!(state.selected_tab(), OperatorTab::RebalanceHedge);

        assert_eq!(state.apply_input(TuiInput::NextTab), None);
        assert_eq!(state.selected_tab(), OperatorTab::Overview);

        for expected in [
            OperatorTab::Rfqs,
            OperatorTab::Liquidity,
            OperatorTab::Risk,
            OperatorTab::LedgerPnl,
            OperatorTab::RebalanceHedge,
        ] {
            assert_eq!(state.apply_input(TuiInput::NextTab), None);
            assert_eq!(state.selected_tab(), expected);
        }
    }

    #[test]
    fn tui_state_quit_is_a_graceful_command() {
        let mut state = TuiState::default();

        let command = state.apply_input(TuiInput::Quit);

        assert_eq!(command, Some(OperatorCommand::Quit));
        assert!(state.should_quit());
    }

    #[test]
    fn tui_state_demo_actions_are_typed_commands() {
        let mut state = TuiState::default();

        let command = state.apply_input(TuiInput::Demo(
            ScriptedDemoAction::TriggerGatewayRefillCheck,
        ));

        assert_eq!(
            command,
            Some(OperatorCommand::Demo(
                ScriptedDemoAction::TriggerGatewayRefillCheck
            ))
        );
        assert!(!state.should_quit());
    }

    #[test]
    fn tui_state_row_selection_clamps_to_view_rows() {
        let mut state = TuiState::default();
        state.select_previous_row(3);

        assert_eq!(state.selected_row_for(OperatorTab::Overview, 3), 2);
        assert_eq!(state.selected_row_for(OperatorTab::Overview, 2), 1);
        assert_eq!(state.selected_row_for(OperatorTab::Overview, 0), 0);
    }

    #[test]
    fn tui_view_model_projects_quote_events_for_rfq_tab() {
        let quote_id = crate::types::QuoteId::generate();
        let run_id = RuntimeRunId::generate();
        let runtime = runtime_with_event(RuntimeEvent::Quote(QuoteEvent::Requested {
            metadata: EventMetadata::new(run_id),
            quote_id,
            pair: AssetPair::new(AssetId::from("USDC"), AssetId::from("SOL")),
            input_amount: TokenAmount::new(AssetId::from("USDC"), AmountRaw::new(1_000_000)),
            taker_wallet: WalletAddress::new("taker111"),
            expires_at: OffsetDateTime::now_utc(),
        }));

        let view = TuiViewModel::from_runtime(&runtime);

        assert!(view.event_rows.iter().any(|row| row.label == "RFQ"));
        assert!(view.event_rows.iter().any(|row| row.status == "requested"));
        assert!(
            view.event_rows
                .iter()
                .any(|row| row.value.contains("USDC->SOL"))
        );
    }

    #[test]
    fn tui_view_model_exposes_selected_placeholder_status_rows() {
        let mut runtime = empty_runtime();
        runtime.rfq = RfqProjection {
            active_quote_count: 2,
            accepted_quote_count: 1,
            rejected_quote_count: 1,
            active_settlement_count: 1,
        };
        runtime.risk = RiskProjection {
            last_decision: Some(RiskDecision::Rejected {
                reason: RejectionReason::InventoryBelowQuoteableThreshold,
                details: vec!["inventory below quoteable threshold".to_owned()],
            }),
            active_rejections: vec![RejectionReason::InventoryBelowQuoteableThreshold],
            require_taker_allowlist: true,
            max_price_staleness_seconds: 20,
        };
        runtime.rebalance = RebalanceProjection {
            pending_swap_count: 1,
            completed_swap_count: 2,
            pending_hedge_count: 1,
            completed_hedge_count: 0,
        };

        let view = TuiViewModel::from_runtime(&runtime);

        assert_eq!(view.rfq_rows[0].label, "Active quotes");
        assert_eq!(view.rfq_rows[0].status, "active");
        assert_eq!(view.risk_rows[0].label, "Taker allowlist");
        assert_eq!(view.risk_rows[0].status, "required");
        assert!(view.risk_rows.iter().any(|row| row.status == "rejected"
            && row.value.contains("inventory below quoteable threshold")));
        assert_eq!(view.rebalance_rows[0].label, "Pending swaps");
        assert_eq!(view.rebalance_rows[0].status, "pending");
    }

    fn runtime_with_event(event: RuntimeEvent) -> RuntimeState {
        let mut runtime = empty_runtime();
        runtime.recent_events.push(event);
        runtime
    }

    fn empty_runtime() -> RuntimeState {
        RuntimeState {
            run_id: RuntimeRunId::generate(),
            started_at: OffsetDateTime::now_utc(),
            inventory: InventoryProjection {
                balances: Vec::new(),
                quoteable_thresholds: Vec::new(),
                max_drift_bps: 0,
                status: "empty".to_owned(),
            },
            risk: RiskProjection {
                last_decision: None,
                active_rejections: Vec::new(),
                require_taker_allowlist: false,
                max_price_staleness_seconds: 20,
            },
            pnl: PnlProjection {
                realized_spread_usdc_estimate: Decimal::ZERO,
                fees_usdc_estimate: Decimal::ZERO,
                hedge_cost_usdc_estimate: Decimal::ZERO,
                rebalance_cost_usdc_estimate: Decimal::ZERO,
                net_usdc_estimate: Decimal::ZERO,
            },
            rfq: RfqProjection::default(),
            rebalance: RebalanceProjection::default(),
            gateway: GatewayProjection {
                enabled: false,
                usdc_refill_threshold_raw: AmountRaw::new(0),
                usdc_refill_target_raw: AmountRaw::new(0),
                status: "not_checked".to_owned(),
            },
            recent_events: Vec::new(),
        }
    }
}
