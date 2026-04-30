//! Spot hedge decisions for SOL and cbBTC exposure.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use time::OffsetDateTime;

use crate::config::AppConfig;
use crate::inventory::InventorySnapshot;
use crate::rebalance::{
    AutomationActionKind, AutomationLimits, AutomationState, DecisionBlockReason, PlannedSwap,
    SwapPlanInput, plan_capped_swap,
};
use crate::types::{AssetId, AssetPair};

/// Spot hedge exposure policy.
#[derive(Debug, Clone, PartialEq)]
pub struct HedgePolicy {
    /// Stable asset to trade against.
    pub usdc_asset: AssetId,
    /// Per-asset absolute exposure limit in USD.
    pub exposure_limit_usd_by_asset: BTreeMap<AssetId, Decimal>,
}

impl HedgePolicy {
    /// Build conservative limits from config.
    #[must_use]
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            usdc_asset: AssetId::from("USDC"),
            exposure_limit_usd_by_asset: BTreeMap::from([
                (AssetId::from("SOL"), config.risk.max_trade_notional_usd),
                (AssetId::from("cbBTC"), config.risk.max_cbbtc_notional_usd),
            ]),
        }
    }
}

/// Hedge decision.
#[derive(Debug, Clone, PartialEq)]
pub enum HedgeDecision {
    /// No hedge action should be submitted.
    NoAction { reason: DecisionBlockReason },
    /// Submit a Jupiter spot hedge swap.
    Swap(PlannedSwap),
}

/// Decide the next spot hedge from current volatile inventory exposure.
#[must_use]
pub fn decide_hedge(
    snapshot: &InventorySnapshot,
    state: &AutomationState,
    limits: &AutomationLimits,
    policy: &HedgePolicy,
    now: OffsetDateTime,
) -> HedgeDecision {
    let Some(position) = snapshot
        .positions
        .values()
        .filter(|position| position.asset != policy.usdc_asset)
        .filter_map(|position| {
            let limit = policy.exposure_limit_usd_by_asset.get(&position.asset)?;
            let excess = position.value_usd - *limit;
            (excess >= Decimal::ZERO).then_some((position, excess))
        })
        .max_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| left.0.asset.cmp(&right.0.asset).reverse())
        })
    else {
        return HedgeDecision::NoAction {
            reason: DecisionBlockReason::NoExposure,
        };
    };

    match plan_capped_swap(SwapPlanInput {
        snapshot,
        kind: AutomationActionKind::Hedge,
        pair: AssetPair::new(position.0.asset.clone(), policy.usdc_asset.clone()),
        desired_notional_usd: position.1,
        state,
        limits,
        now,
        reason: "spot exposure hedge".to_owned(),
    }) {
        Ok(plan) => HedgeDecision::Swap(plan),
        Err(reason) => HedgeDecision::NoAction { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::{InventoryPolicy, InventorySnapshot};
    use crate::rebalance::{AutomationLimits, AutomationState};
    use crate::types::{AmountRaw, AssetId, AssetPair, BalanceSnapshot, TokenAmount, WalletRole};

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

    fn snapshot(balances: Vec<TokenAmount>) -> InventorySnapshot {
        let config = AppConfig::default();
        InventorySnapshot::from_balance_snapshot(
            &config,
            BalanceSnapshot {
                wallet: WalletRole::Maker,
                balances,
                observed_at: OffsetDateTime::UNIX_EPOCH,
            },
            &prices(),
            &InventoryPolicy::from_config(&config),
        )
        .expect("inventory snapshot")
    }

    fn defaults() -> (AutomationLimits, HedgePolicy) {
        let config = AppConfig::default();
        let inventory_policy = InventoryPolicy::from_config(&config);
        let limits = AutomationLimits::from_config(&config, &inventory_policy);
        let policy = HedgePolicy::from_config(&config);
        (limits, policy)
    }

    #[test]
    fn hedge_decision_sells_sol_when_exposure_exceeds_limit() {
        let (limits, policy) = defaults();
        let decision = decide_hedge(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(1_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(100_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &AutomationState::default(),
            &limits,
            &policy,
            OffsetDateTime::UNIX_EPOCH,
        );

        match decision {
            HedgeDecision::Swap(plan) => {
                assert_eq!(plan.kind, AutomationActionKind::Hedge);
                assert_eq!(plan.pair, AssetPair::new(asset("SOL"), asset("USDC")));
                assert!(plan.estimated_notional_usd <= Decimal::from(2));
            }
            other @ HedgeDecision::NoAction { .. } => {
                panic!("expected hedge swap, got {other:?}");
            }
        }
    }

    #[test]
    fn hedge_decision_has_no_action_when_exposure_is_under_limit() {
        let (limits, policy) = defaults();
        let decision = decide_hedge(
            &snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(10_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(10_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &AutomationState::default(),
            &limits,
            &policy,
            OffsetDateTime::UNIX_EPOCH,
        );

        assert_eq!(
            decision,
            HedgeDecision::NoAction {
                reason: DecisionBlockReason::NoExposure
            }
        );
    }
}
