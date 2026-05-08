//! Rebalance and refill decision policy for inventory-first RFQ fills.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use time::{Duration, OffsetDateTime};

use crate::config::{AppConfig, GatewayConfig};
use crate::domain::events::{EventMetadata, SwapEvent};
use crate::domain::inventory::{
    InventoryPolicy, InventorySnapshot, raw_to_units, units_to_raw_floor,
};
use crate::domain::types::{
    AmountRaw, AssetId, AssetPair, GatewayReceipt, GatewayRefillRequest, RuntimeRunId, SwapQuote,
    SwapReceipt, SwapRequest, TokenAmount, WalletRole,
};
use crate::error::AppResult;
use crate::ports::{GatewayClient, SwapExecutor};

/// Automation action category used for concurrency and cooldown keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AutomationActionKind {
    /// Jupiter inventory rebalance.
    Rebalance,
    /// Circle Gateway USDC refill.
    GatewayRefill,
    /// Circle Gateway USDC deposit from excess working custody.
    GatewayDeposit,
    /// Jupiter USDC to SOL native top-up.
    NativeSolTopUp,
}

/// Deduplication key for in-flight and cooldown checks.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionKey {
    /// Action category.
    pub kind: AutomationActionKind,
    /// Source asset for the action.
    pub source_asset: AssetId,
    /// Destination asset for the action.
    pub dest_asset: AssetId,
}

/// Hard automation limits for the demo runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct AutomationLimits {
    /// Default max USD notional per action.
    pub max_action_notional_usd: Decimal,
    /// Max USD notional over this process run.
    pub max_cumulative_automation_notional_usd: Decimal,
    /// One-off routeability exception cap for non-native, non-stable assets.
    pub non_stable_asset_exception_notional_usd: Decimal,
    /// Max concurrent automated actions.
    pub max_concurrent_actions: usize,
    /// Cooldown between duplicate actions.
    pub cooldown: Duration,
    /// Swap slippage cap.
    pub max_slippage_bps: u16,
    /// SOL gas buffer retained by policy.
    pub native_sol_gas_buffer_raw: AmountRaw,
}

impl AutomationLimits {
    /// Build limits from safe config defaults.
    #[must_use]
    pub fn from_config(config: &AppConfig, inventory_policy: &InventoryPolicy) -> Self {
        Self {
            max_action_notional_usd: config.assets.policy.max_action_notional_usd,
            max_cumulative_automation_notional_usd: config
                .assets
                .policy
                .max_cumulative_automation_notional_usd,
            non_stable_asset_exception_notional_usd: config
                .assets
                .policy
                .non_stable_asset_exception_notional_usd,
            max_concurrent_actions: 1,
            cooldown: Duration::seconds(30),
            max_slippage_bps: config.jupiter.max_slippage_bps,
            native_sol_gas_buffer_raw: inventory_policy.native_sol_gas_buffer_raw,
        }
    }
}

/// Mutable automation accounting supplied by the runtime projection.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AutomationState {
    /// Cumulative automated notional already submitted in this run.
    pub cumulative_spend_usd: Decimal,
    /// Actions currently in flight.
    pub in_flight: Vec<ActionKey>,
    /// Last submission timestamp by action key.
    pub last_submitted_at: BTreeMap<ActionKey, OffsetDateTime>,
    /// Whether the one-off non-stable asset exception has already been used.
    pub non_stable_asset_exception_used: bool,
}

/// Stable no-action reasons for tests, API projection, and operator copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionBlockReason {
    /// No configured imbalance warrants action.
    NoDrift,
    /// Configured concurrency limit is already reached.
    MaxConcurrentActions,
    /// Same action is already in flight.
    DuplicateInFlight,
    /// Same action was submitted too recently.
    CooldownActive,
    /// The run-level cumulative cap would be exceeded.
    CumulativeCapExceeded,
    /// SOL gas buffer would be consumed or is unavailable.
    NativeGasBufferInsufficient,
    /// Source inventory is insufficient after reserves.
    InsufficientInventory,
    /// Asset metadata or price is missing.
    UnsupportedAsset,
    /// The capped raw input amount rounds to zero.
    ZeroAmount,
}

/// A planned Jupiter swap action.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedSwap {
    /// Action category.
    pub kind: AutomationActionKind,
    /// Directional swap pair.
    pub pair: AssetPair,
    /// Raw input amount.
    pub input_amount: TokenAmount,
    /// Estimated USD notional spent by this action.
    pub estimated_notional_usd: Decimal,
    /// Whether this action consumes the one-off non-stable asset exception.
    pub uses_non_stable_asset_exception: bool,
    /// Operator-facing reason.
    pub reason: String,
}

impl PlannedSwap {
    /// Return the action key used for guards.
    #[must_use]
    pub fn action_key(&self) -> ActionKey {
        ActionKey {
            kind: self.kind,
            source_asset: self.pair.input.clone(),
            dest_asset: self.pair.output.clone(),
        }
    }

    /// Build the adapter request for this planned swap.
    #[must_use]
    pub fn swap_request(&self, max_slippage_bps: u16) -> SwapRequest {
        SwapRequest {
            pair: self.pair.clone(),
            input_amount: self.input_amount.clone(),
            source_wallet: WalletRole::Maker,
            destination_wallet: WalletRole::Maker,
            max_slippage_bps,
        }
    }
}

/// Rebalance decision result.
#[derive(Debug, Clone, PartialEq)]
pub enum RebalanceDecision {
    /// No action should be submitted.
    NoAction { reason: DecisionBlockReason },
    /// Submit a Jupiter swap.
    Swap(PlannedSwap),
}

/// Gateway refill decision result.
#[derive(Debug, Clone, PartialEq)]
pub enum GatewayRefillDecision {
    /// No Gateway action should be submitted.
    NoAction { reason: DecisionBlockReason },
    /// Submit a Gateway refill request.
    Refill(GatewayRefillPlan),
}

/// Planned Circle Gateway refill.
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayRefillPlan {
    /// Gateway refill request.
    pub request: GatewayRefillRequest,
    /// Estimated USD notional.
    pub estimated_notional_usd: Decimal,
}

/// Planned Circle Gateway excess deposit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcessDepositPlan {
    /// Raw USDC amount to move from working custody into Gateway.
    pub amount_raw: AmountRaw,
}

/// Rebalance thresholds and stable asset ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebalancePolicy {
    /// Drift threshold in basis points.
    pub drift_threshold_bps: i32,
    /// Stable USDC asset id.
    pub usdc_asset: AssetId,
    /// Native SOL asset id.
    pub sol_asset: AssetId,
    /// Native SOL top-up target.
    pub native_sol_top_up_target_raw: AmountRaw,
}

impl RebalancePolicy {
    /// Build from config.
    #[must_use]
    pub fn from_config(config: &AppConfig, inventory_policy: &InventoryPolicy) -> Self {
        Self {
            drift_threshold_bps: i32::from(config.risk.max_inventory_drift_bps),
            usdc_asset: AssetId::from("USDC"),
            sol_asset: AssetId::from("SOL"),
            native_sol_top_up_target_raw: AmountRaw::new(
                inventory_policy
                    .native_sol_gas_buffer_raw
                    .as_u64()
                    .saturating_mul(2),
            ),
        }
    }
}

/// Decide the next rebalance action.
#[must_use]
pub fn decide_rebalance(
    snapshot: &InventorySnapshot,
    state: &AutomationState,
    limits: &AutomationLimits,
    policy: &RebalancePolicy,
    now: OffsetDateTime,
) -> RebalanceDecision {
    let inventory_policy = inventory_policy_from_limits(limits);
    if !snapshot.has_native_sol_gas_buffer(&inventory_policy) {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::NativeGasBufferInsufficient,
        };
    }

    match decide_native_sol_top_up(snapshot, state, limits, policy, now) {
        RebalanceDecision::Swap(plan) => return RebalanceDecision::Swap(plan),
        RebalanceDecision::NoAction {
            reason: DecisionBlockReason::NoDrift,
        } => {}
        no_action @ RebalanceDecision::NoAction { .. } => return no_action,
    }

    decide_inventory_rebalance(snapshot, state, limits, policy, now)
}

/// Decide the next non-top-up inventory rebalance action.
#[must_use]
pub fn decide_inventory_rebalance(
    snapshot: &InventorySnapshot,
    state: &AutomationState,
    limits: &AutomationLimits,
    policy: &RebalancePolicy,
    now: OffsetDateTime,
) -> RebalanceDecision {
    let inventory_policy = inventory_policy_from_limits(limits);
    if !snapshot.has_native_sol_gas_buffer(&inventory_policy) {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::NativeGasBufferInsufficient,
        };
    }

    let Some(source) = snapshot
        .positions
        .values()
        .filter(|position| position.drift_bps >= policy.drift_threshold_bps)
        .max_by(|left, right| {
            left.excess_value_usd()
                .cmp(&right.excess_value_usd())
                .then_with(|| left.asset.cmp(&right.asset).reverse())
        })
    else {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::NoDrift,
        };
    };

    let Some(dest) = snapshot
        .positions
        .values()
        .filter(|position| position.drift_bps <= -policy.drift_threshold_bps)
        .max_by(|left, right| {
            left.deficit_value_usd()
                .cmp(&right.deficit_value_usd())
                .then_with(|| left.asset.cmp(&right.asset).reverse())
        })
    else {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::NoDrift,
        };
    };

    let desired_notional = source.excess_value_usd().min(dest.deficit_value_usd());
    match plan_capped_swap(SwapPlanInput {
        snapshot,
        kind: AutomationActionKind::Rebalance,
        pair: AssetPair::new(source.asset.clone(), dest.asset.clone()),
        desired_notional_usd: desired_notional,
        state,
        limits,
        now,
        reason: "inventory drift correction".to_owned(),
    }) {
        Ok(plan) => RebalanceDecision::Swap(plan),
        Err(reason) => RebalanceDecision::NoAction { reason },
    }
}

/// Decide whether the maker wallet needs a native SOL top-up.
#[must_use]
pub fn decide_native_sol_top_up(
    snapshot: &InventorySnapshot,
    state: &AutomationState,
    limits: &AutomationLimits,
    policy: &RebalancePolicy,
    now: OffsetDateTime,
) -> RebalanceDecision {
    let Some(sol_position) = snapshot.position(&policy.sol_asset) else {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::UnsupportedAsset,
        };
    };
    if sol_position.balance_raw.as_u64() >= policy.native_sol_top_up_target_raw.as_u64() {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::NoDrift,
        };
    }

    let shortfall_raw = AmountRaw::new(
        policy
            .native_sol_top_up_target_raw
            .as_u64()
            .saturating_sub(sol_position.balance_raw.as_u64()),
    );
    let desired_notional = raw_to_units(shortfall_raw, sol_position.decimals)
        * sol_position.price_usd.max(Decimal::ZERO);

    if desired_notional <= Decimal::ZERO {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::ZeroAmount,
        };
    }

    let pair = AssetPair::new(policy.usdc_asset.clone(), policy.sol_asset.clone());
    let key = ActionKey {
        kind: AutomationActionKind::NativeSolTopUp,
        source_asset: pair.input.clone(),
        dest_asset: pair.output.clone(),
    };
    if let Err(reason) = guard_action(state, limits, &key, now) {
        return RebalanceDecision::NoAction { reason };
    }

    let inventory_policy = inventory_policy_from_limits(limits);
    if !snapshot.has_native_sol_gas_buffer(&inventory_policy) {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::NativeGasBufferInsufficient,
        };
    }

    let Some(source) = snapshot.position(&pair.input) else {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::UnsupportedAsset,
        };
    };
    if source.price_usd <= Decimal::ZERO {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::UnsupportedAsset,
        };
    }

    let (capped_notional, uses_non_stable_asset_exception) =
        cap_notional_for_pair(&pair, desired_notional, state, limits);
    if state.cumulative_spend_usd + capped_notional > limits.max_cumulative_automation_notional_usd
    {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::CumulativeCapExceeded,
        };
    }

    let requested_raw = units_to_raw_floor(capped_notional / source.price_usd, source.decimals);
    if requested_raw.is_zero() {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::ZeroAmount,
        };
    }

    let estimated_notional_usd = raw_to_units(requested_raw, source.decimals) * source.price_usd;
    if estimated_notional_usd <= Decimal::ZERO {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::ZeroAmount,
        };
    }
    if state.cumulative_spend_usd + estimated_notional_usd
        > limits.max_cumulative_automation_notional_usd
    {
        return RebalanceDecision::NoAction {
            reason: DecisionBlockReason::CumulativeCapExceeded,
        };
    }

    RebalanceDecision::Swap(PlannedSwap {
        kind: AutomationActionKind::NativeSolTopUp,
        pair: pair.clone(),
        input_amount: TokenAmount::new(pair.input, requested_raw),
        estimated_notional_usd,
        uses_non_stable_asset_exception,
        reason: "native SOL gas top-up".to_owned(),
    })
}

/// Decide whether the working wallet needs a Gateway USDC refill.
#[must_use]
pub fn decide_gateway_refill(
    snapshot: &InventorySnapshot,
    state: &AutomationState,
    limits: &AutomationLimits,
    config: &AppConfig,
    now: OffsetDateTime,
) -> GatewayRefillDecision {
    let usdc = AssetId::from("USDC");
    let current = snapshot.raw_balance(&usdc);
    if current.as_u64() >= config.gateway.usdc_refill_threshold_raw.as_u64() {
        return GatewayRefillDecision::NoAction {
            reason: DecisionBlockReason::NoDrift,
        };
    }

    let key = ActionKey {
        kind: AutomationActionKind::GatewayRefill,
        source_asset: usdc.clone(),
        dest_asset: usdc.clone(),
    };
    if let Err(reason) = guard_action(state, limits, &key, now) {
        return GatewayRefillDecision::NoAction { reason };
    }

    let raw_shortfall = config
        .gateway
        .usdc_refill_target_raw
        .as_u64()
        .saturating_sub(current.as_u64());
    let target_notional = raw_to_units(AmountRaw::new(raw_shortfall), 6)
        .min(config.gateway.max_refill_notional_usd)
        .min(limits.max_action_notional_usd);

    if state.cumulative_spend_usd + target_notional > limits.max_cumulative_automation_notional_usd
    {
        return GatewayRefillDecision::NoAction {
            reason: DecisionBlockReason::CumulativeCapExceeded,
        };
    }

    let amount = TokenAmount::new(usdc, units_to_raw_floor(target_notional, 6));
    if amount.amount_raw.is_zero() {
        return GatewayRefillDecision::NoAction {
            reason: DecisionBlockReason::ZeroAmount,
        };
    }

    GatewayRefillDecision::Refill(GatewayRefillPlan {
        request: GatewayRefillRequest {
            amount,
            destination: WalletRole::Maker,
        },
        estimated_notional_usd: target_notional,
    })
}

/// Decide whether excess working USDC should be deposited into Gateway.
#[must_use]
pub fn decide_excess_deposit(
    snapshot: &InventorySnapshot,
    config: &GatewayConfig,
) -> Option<ExcessDepositPlan> {
    if !config.enabled {
        return None;
    }

    let usdc = AssetId::from("USDC");
    let working_usdc = snapshot.raw_balance(&usdc);
    if working_usdc.as_u64() <= config.usdc_excess_deposit_threshold_raw.as_u64() {
        return None;
    }

    let excess = working_usdc
        .as_u64()
        .saturating_sub(config.usdc_excess_deposit_target_raw.as_u64());
    if excess == 0 {
        return None;
    }

    Some(ExcessDepositPlan {
        amount_raw: AmountRaw::new(excess),
    })
}

/// Inputs for shared capped swap planning.
pub struct SwapPlanInput<'a> {
    /// Inventory snapshot used for prices and balances.
    pub snapshot: &'a InventorySnapshot,
    /// Automation action category.
    pub kind: AutomationActionKind,
    /// Directional swap pair.
    pub pair: AssetPair,
    /// Desired USD notional before caps.
    pub desired_notional_usd: Decimal,
    /// Current automation state.
    pub state: &'a AutomationState,
    /// Hard automation limits.
    pub limits: &'a AutomationLimits,
    /// Current decision time.
    pub now: OffsetDateTime,
    /// Operator-facing reason.
    pub reason: String,
}

/// Shared helper used by rebalance decisions.
///
/// # Errors
///
/// Returns a stable no-action reason when guards, caps, gas reserves, or
/// inventory availability prevent a swap.
pub fn plan_capped_swap(input: SwapPlanInput<'_>) -> Result<PlannedSwap, DecisionBlockReason> {
    let SwapPlanInput {
        snapshot,
        kind,
        pair,
        desired_notional_usd,
        state,
        limits,
        now,
        reason,
    } = input;

    if desired_notional_usd <= Decimal::ZERO {
        return Err(DecisionBlockReason::ZeroAmount);
    }

    let key = ActionKey {
        kind,
        source_asset: pair.input.clone(),
        dest_asset: pair.output.clone(),
    };
    guard_action(state, limits, &key, now)?;

    let inventory_policy = inventory_policy_from_limits(limits);
    if pair.input.as_str() != "SOL" && !snapshot.has_native_sol_gas_buffer(&inventory_policy) {
        return Err(DecisionBlockReason::NativeGasBufferInsufficient);
    }

    let source = snapshot
        .position(&pair.input)
        .ok_or(DecisionBlockReason::UnsupportedAsset)?;
    if source.price_usd <= Decimal::ZERO {
        return Err(DecisionBlockReason::UnsupportedAsset);
    }

    let (capped_notional, uses_non_stable_asset_exception) =
        cap_notional_for_pair(&pair, desired_notional_usd, state, limits);
    if state.cumulative_spend_usd + capped_notional > limits.max_cumulative_automation_notional_usd
    {
        return Err(DecisionBlockReason::CumulativeCapExceeded);
    }

    let requested_raw = units_to_raw_floor(capped_notional / source.price_usd, source.decimals);
    let available_raw = snapshot.source_available_raw(&pair.input, &inventory_policy);
    let input_raw = AmountRaw::new(requested_raw.as_u64().min(available_raw.as_u64()));
    if input_raw.is_zero() {
        return Err(if pair.input.as_str() == "SOL" {
            DecisionBlockReason::NativeGasBufferInsufficient
        } else {
            DecisionBlockReason::InsufficientInventory
        });
    }

    let estimated_notional_usd = raw_to_units(input_raw, source.decimals) * source.price_usd;
    if estimated_notional_usd <= Decimal::ZERO {
        return Err(DecisionBlockReason::ZeroAmount);
    }
    if state.cumulative_spend_usd + estimated_notional_usd
        > limits.max_cumulative_automation_notional_usd
    {
        return Err(DecisionBlockReason::CumulativeCapExceeded);
    }

    Ok(PlannedSwap {
        kind,
        pair: pair.clone(),
        input_amount: TokenAmount::new(pair.input, input_raw),
        estimated_notional_usd,
        uses_non_stable_asset_exception,
        reason,
    })
}

fn guard_action(
    state: &AutomationState,
    limits: &AutomationLimits,
    key: &ActionKey,
    now: OffsetDateTime,
) -> Result<(), DecisionBlockReason> {
    if state.in_flight.len() >= limits.max_concurrent_actions {
        return Err(DecisionBlockReason::MaxConcurrentActions);
    }
    if state.in_flight.iter().any(|in_flight| in_flight == key) {
        return Err(DecisionBlockReason::DuplicateInFlight);
    }
    if state
        .last_submitted_at
        .get(key)
        .is_some_and(|last| now - *last < limits.cooldown)
    {
        return Err(DecisionBlockReason::CooldownActive);
    }
    Ok(())
}

fn cap_notional_for_pair(
    pair: &AssetPair,
    desired_notional_usd: Decimal,
    state: &AutomationState,
    limits: &AutomationLimits,
) -> (Decimal, bool) {
    let touches_non_stable_asset =
        is_non_native_non_stable_asset(&pair.input) || is_non_native_non_stable_asset(&pair.output);
    if touches_non_stable_asset
        && desired_notional_usd > limits.max_action_notional_usd
        && !state.non_stable_asset_exception_used
    {
        (
            desired_notional_usd.min(limits.non_stable_asset_exception_notional_usd),
            true,
        )
    } else {
        (
            desired_notional_usd.min(limits.max_action_notional_usd),
            false,
        )
    }
}

fn is_non_native_non_stable_asset(asset: &AssetId) -> bool {
    !matches!(asset.as_str(), "USDC" | "SOL")
}

fn inventory_policy_from_limits(limits: &AutomationLimits) -> InventoryPolicy {
    InventoryPolicy {
        native_sol_gas_buffer_raw: limits.native_sol_gas_buffer_raw,
    }
}

/// Submit a planned swap through the adapter trait.
///
/// # Errors
///
/// Returns any quote or execution error from the adapter.
pub async fn submit_planned_swap(
    executor: &(impl SwapExecutor + ?Sized),
    plan: &PlannedSwap,
    limits: &AutomationLimits,
    run_id: RuntimeRunId,
) -> AppResult<SwapSubmission> {
    let quote = executor
        .quote_swap(plan.swap_request(limits.max_slippage_bps))
        .await?;
    let quoted_event = SwapEvent::Quoted {
        metadata: EventMetadata::new(run_id),
        quote: quote.clone(),
    };
    let receipt = executor.execute_swap(quote.clone()).await?;
    let executed_event = SwapEvent::Executed {
        metadata: EventMetadata::new(run_id),
        receipt: receipt.clone(),
    };

    Ok(SwapSubmission {
        quote,
        receipt,
        events: vec![quoted_event, executed_event],
    })
}

/// Submitted swap artifacts and compatible events.
#[derive(Debug, Clone, PartialEq)]
pub struct SwapSubmission {
    /// Adapter quote.
    pub quote: SwapQuote,
    /// Adapter receipt.
    pub receipt: SwapReceipt,
    /// Runtime events emitted by the worker boundary.
    pub events: Vec<SwapEvent>,
}

/// Submit a Gateway refill through the adapter trait.
///
/// # Errors
///
/// Returns any Gateway adapter error.
pub async fn submit_gateway_refill(
    gateway: &(impl GatewayClient + ?Sized),
    plan: &GatewayRefillPlan,
) -> AppResult<GatewayReceipt> {
    gateway.request_refill(plan.request.clone()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    use crate::domain::inventory::InventorySnapshot;
    use crate::domain::types::{BalanceSnapshot, SwapReceipt, TxSignature};
    use crate::error::AppError;

    fn asset(symbol: &str) -> AssetId {
        AssetId::from(symbol)
    }

    fn prices() -> BTreeMap<AssetId, Decimal> {
        BTreeMap::from([
            (asset("USDC"), Decimal::ONE),
            (asset("SOL"), Decimal::from(100)),
            (asset("cbBTC"), Decimal::from(50_000)),
        ])
    }

    fn balance_snapshot(balances: Vec<TokenAmount>) -> BalanceSnapshot {
        BalanceSnapshot {
            wallet: WalletRole::Maker,
            balances,
            observed_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn snapshot(balances: Vec<TokenAmount>) -> InventorySnapshot {
        let config = AppConfig::default();
        InventorySnapshot::from_balance_snapshot(
            &config,
            balance_snapshot(balances),
            &prices(),
            &InventoryPolicy::from_config(&config),
        )
        .expect("inventory snapshot")
    }

    fn defaults() -> (AutomationLimits, RebalancePolicy) {
        let config = AppConfig::default();
        let inventory_policy = InventoryPolicy::from_config(&config);
        let limits = AutomationLimits::from_config(&config, &inventory_policy);
        let policy = RebalancePolicy::from_config(&config, &inventory_policy);
        (limits, policy)
    }

    #[test]
    fn rebalance_decision_sells_overweight_asset_to_underweight_asset() {
        let (limits, policy) = defaults();
        let decision = decide_rebalance(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(10_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(100_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &AutomationState::default(),
            &limits,
            &policy,
            OffsetDateTime::UNIX_EPOCH,
        );

        match decision {
            RebalanceDecision::Swap(plan) => {
                assert_eq!(plan.pair, AssetPair::new(asset("SOL"), asset("USDC")));
                assert!(plan.estimated_notional_usd <= Decimal::from(2));
            }
            other @ RebalanceDecision::NoAction { .. } => {
                panic!("expected rebalance swap, got {other:?}");
            }
        }
    }

    #[test]
    fn rebalance_native_sol_gas_buffer_blocks_sol_spend() {
        let (limits, policy) = defaults();
        let decision = decide_rebalance(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(1_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(5_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &AutomationState::default(),
            &limits,
            &policy,
            OffsetDateTime::UNIX_EPOCH,
        );

        assert_eq!(
            decision,
            RebalanceDecision::NoAction {
                reason: DecisionBlockReason::NativeGasBufferInsufficient
            }
        );
    }

    #[test]
    fn rebalance_max_concurrent_actions_blocks_new_action() {
        let (limits, policy) = defaults();
        let state = AutomationState {
            in_flight: vec![ActionKey {
                kind: AutomationActionKind::Rebalance,
                source_asset: asset("SOL"),
                dest_asset: asset("USDC"),
            }],
            ..AutomationState::default()
        };

        let decision = decide_rebalance(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(10_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(100_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &state,
            &limits,
            &policy,
            OffsetDateTime::UNIX_EPOCH,
        );

        assert_eq!(
            decision,
            RebalanceDecision::NoAction {
                reason: DecisionBlockReason::MaxConcurrentActions
            }
        );
    }

    #[test]
    fn rebalance_cooldown_blocks_duplicate_recent_action() {
        let (limits, policy) = defaults();
        let key = ActionKey {
            kind: AutomationActionKind::Rebalance,
            source_asset: asset("SOL"),
            dest_asset: asset("USDC"),
        };
        let state = AutomationState {
            last_submitted_at: BTreeMap::from([(key, OffsetDateTime::UNIX_EPOCH)]),
            ..AutomationState::default()
        };

        let decision = decide_rebalance(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(10_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(100_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &state,
            &limits,
            &policy,
            OffsetDateTime::UNIX_EPOCH + Duration::seconds(10),
        );

        assert_eq!(
            decision,
            RebalanceDecision::NoAction {
                reason: DecisionBlockReason::CooldownActive
            }
        );
    }

    #[test]
    fn rebalance_cumulative_cap_blocks_action_that_would_exceed_run_cap() {
        let (limits, policy) = defaults();
        let state = AutomationState {
            cumulative_spend_usd: Decimal::from(14),
            ..AutomationState::default()
        };

        let decision = decide_rebalance(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(10_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(100_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &state,
            &limits,
            &policy,
            OffsetDateTime::UNIX_EPOCH,
        );

        assert_eq!(
            decision,
            RebalanceDecision::NoAction {
                reason: DecisionBlockReason::CumulativeCapExceeded
            }
        );
    }

    #[test]
    fn rebalance_non_stable_asset_exception_allows_one_action_above_default_cap() {
        let (limits, policy) = defaults();
        let decision = decide_rebalance(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(1_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(20_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(10_000)),
            ]),
            &AutomationState::default(),
            &limits,
            &policy,
            OffsetDateTime::UNIX_EPOCH,
        );

        match decision {
            RebalanceDecision::Swap(plan) => {
                assert_eq!(plan.pair, AssetPair::new(asset("cbBTC"), asset("USDC")));
                assert!(plan.estimated_notional_usd > Decimal::from(2));
                assert!(plan.estimated_notional_usd <= Decimal::from(5));
                assert!(plan.uses_non_stable_asset_exception);
            }
            other @ RebalanceDecision::NoAction { .. } => {
                panic!("expected non-stable asset exception swap, got {other:?}");
            }
        }
    }

    #[test]
    fn rebalance_no_action_when_inventory_is_inside_drift_band() {
        let (limits, policy) = defaults();
        let decision = decide_rebalance(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(6_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(35_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(100)),
            ]),
            &AutomationState::default(),
            &limits,
            &policy,
            OffsetDateTime::UNIX_EPOCH,
        );

        assert_eq!(
            decision,
            RebalanceDecision::NoAction {
                reason: DecisionBlockReason::NoDrift
            }
        );
    }

    #[test]
    fn excess_deposit_decision_drains_working_usdc_to_target() {
        let mut config = AppConfig::default();
        config.gateway.usdc_excess_deposit_threshold_raw = AmountRaw::new(12_000_000);
        config.gateway.usdc_excess_deposit_target_raw = AmountRaw::new(10_000_000);

        let plan = decide_excess_deposit(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(15_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(100_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &config.gateway,
        )
        .expect("excess deposit should be planned");

        assert_eq!(plan.amount_raw, AmountRaw::new(5_000_000));
    }

    #[test]
    fn excess_deposit_decision_skips_at_or_below_threshold() {
        let mut config = AppConfig::default();
        config.gateway.usdc_excess_deposit_threshold_raw = AmountRaw::new(12_000_000);
        config.gateway.usdc_excess_deposit_target_raw = AmountRaw::new(10_000_000);

        let plan = decide_excess_deposit(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(12_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(100_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &config.gateway,
        );

        assert!(plan.is_none());
    }

    #[derive(Default)]
    struct FakeSwapExecutor {
        quoted: Arc<Mutex<Vec<SwapRequest>>>,
        executed: Arc<Mutex<Vec<SwapQuote>>>,
    }

    #[async_trait]
    impl SwapExecutor for FakeSwapExecutor {
        async fn quote_swap(&self, request: SwapRequest) -> Result<SwapQuote, AppError> {
            self.quoted
                .lock()
                .expect("quote lock")
                .push(request.clone());
            Ok(SwapQuote {
                expected_output: TokenAmount::new(request.pair.output.clone(), AmountRaw::new(10)),
                estimated_fee: None,
                expires_at: None,
                request,
            })
        }

        async fn execute_swap(&self, quote: SwapQuote) -> Result<SwapReceipt, AppError> {
            self.executed
                .lock()
                .expect("execute lock")
                .push(quote.clone());
            Ok(SwapReceipt {
                trade_id: None,
                signature: TxSignature::new("fake-signature"),
                output_amount: Some(quote.expected_output.clone()),
            })
        }
    }

    #[tokio::test]
    async fn rebalance_fake_swap_executor_action_submission_quotes_and_executes() {
        let (limits, _) = defaults();
        let executor = FakeSwapExecutor::default();
        let plan = PlannedSwap {
            kind: AutomationActionKind::Rebalance,
            pair: AssetPair::new(asset("USDC"), asset("SOL")),
            input_amount: TokenAmount::new(asset("USDC"), AmountRaw::new(1_000_000)),
            estimated_notional_usd: Decimal::ONE,
            uses_non_stable_asset_exception: false,
            reason: "test".to_owned(),
        };

        let submission = submit_planned_swap(&executor, &plan, &limits, RuntimeRunId::generate())
            .await
            .expect("submit planned swap");

        assert_eq!(submission.events.len(), 2);
        assert_eq!(executor.quoted.lock().expect("quote lock").len(), 1);
        assert_eq!(executor.executed.lock().expect("execute lock").len(), 1);
    }
}
