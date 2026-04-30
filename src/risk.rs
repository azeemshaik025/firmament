//! Pure RFQ risk policy evaluation.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::quote_engine::InventorySnapshot;
use crate::types::{
    AmountRaw, AssetId, AssetPair, ReferencePrice, RejectionReason, RiskDecision, TokenAmount,
    WalletAddress,
};

/// Deterministic policy inputs for pre-quote RFQ risk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskPolicy {
    /// Directional pairs enabled for quoting.
    pub supported_pairs: Vec<AssetPair>,
    /// Wallets allowed to request quotes when allowlist mode is enabled.
    pub allowlisted_takers: Vec<WalletAddress>,
    /// Whether takers must be present in the allowlist.
    pub require_taker_allowlist: bool,
    /// Per-RFQ notional cap in estimated USD.
    pub max_quote_notional_usd: Option<Decimal>,
    /// Maximum reference price age.
    pub max_price_staleness_seconds: u64,
    /// Minimum inventory that must remain quoteable by asset.
    pub quoteable_thresholds: Vec<TokenAmount>,
    /// Per-asset exposure limits in estimated USD.
    pub exposure_limits_usd: Vec<(AssetId, Decimal)>,
    /// Current per-asset exposure in estimated USD.
    pub current_exposure_usd: Vec<(AssetId, Decimal)>,
    /// Native SOL balance that must remain available for gas.
    pub native_sol_gas_buffer_raw: crate::types::AmountRaw,
    /// Fallback USD prices for pairs that do not include USDC.
    pub asset_usd_prices: Vec<(AssetId, Decimal)>,
}

/// All data needed for one pre-quote risk evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskEvaluationInput {
    /// Directional pair.
    pub pair: AssetPair,
    /// Taker wallet supplied by the caller.
    pub taker_wallet: WalletAddress,
    /// Taker input amount.
    pub input_amount: TokenAmount,
    /// Maker output amount.
    pub output_amount: TokenAmount,
    /// Reference price used for the quote.
    pub reference_price: ReferencePrice,
    /// Estimated RFQ notional in USD.
    pub notional_usd: Decimal,
    /// Current deterministic clock.
    pub now: OffsetDateTime,
    /// Quote expiry timestamp.
    pub expires_at: OffsetDateTime,
    /// Working inventory snapshot.
    pub inventory: InventorySnapshot,
}

/// Evaluate pre-quote risk and inventory-first acceptance checks.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn evaluate_pre_quote(input: &RiskEvaluationInput, policy: &RiskPolicy) -> RiskDecision {
    let mut checks = Vec::new();

    if !policy
        .supported_pairs
        .iter()
        .any(|pair| pair == &input.pair)
    {
        return rejected(
            RejectionReason::UnsupportedPair,
            format!(
                "pair {} -> {} is not enabled for firm RFQ quoting",
                input.pair.input, input.pair.output
            ),
        );
    }
    checks.push("pair enabled".to_owned());

    if input.expires_at <= input.now {
        return rejected(
            RejectionReason::ValidationFailed,
            "quote expiry must be in the future".to_owned(),
        );
    }
    checks.push("expiry valid".to_owned());

    if policy.require_taker_allowlist
        && !policy
            .allowlisted_takers
            .iter()
            .any(|wallet| wallet == &input.taker_wallet)
    {
        return rejected(
            RejectionReason::WalletNotAllowed,
            format!("taker wallet {} is not allowlisted", input.taker_wallet),
        );
    }
    checks.push("taker allowlist".to_owned());

    let age_seconds = (input.now - input.reference_price.observed_at).whole_seconds();
    let max_staleness = i64::try_from(policy.max_price_staleness_seconds).unwrap_or(i64::MAX);
    if age_seconds > max_staleness {
        return rejected(
            RejectionReason::StalePrice,
            format!(
                "reference price is {age_seconds}s old; max allowed is {}s",
                policy.max_price_staleness_seconds
            ),
        );
    }
    checks.push("reference price fresh".to_owned());

    let Some(max_quote_notional_usd) = policy.max_quote_notional_usd else {
        return rejected(
            RejectionReason::ValidationFailed,
            "missing max_quote_notional_usd risk limit".to_owned(),
        );
    };
    if input.notional_usd > max_quote_notional_usd {
        return rejected(
            RejectionReason::MaxNotionalExceeded,
            format!(
                "RFQ notional {} exceeds per-quote limit {}",
                input.notional_usd, max_quote_notional_usd
            ),
        );
    }
    checks.push("max notional".to_owned());

    if let Some(decision) = exposure_decision(input, policy) {
        return decision;
    }
    checks.push("exposure limits".to_owned());

    let output_balance = input.inventory.balance_for(&input.output_amount.asset);
    if output_balance < input.output_amount.amount_raw {
        return rejected(
            RejectionReason::InventoryBelowQuoteableThreshold,
            format!(
                "working inventory for {} is {}; quote requires {}",
                input.output_amount.asset,
                output_balance.as_u64(),
                input.output_amount.amount_raw.as_u64()
            ),
        );
    }

    let remaining_output = AmountRaw::new(
        output_balance
            .as_u64()
            .saturating_sub(input.output_amount.amount_raw.as_u64()),
    );
    let Some(threshold) = threshold_for(policy, &input.output_amount.asset) else {
        return rejected(
            RejectionReason::ValidationFailed,
            format!(
                "missing quoteable inventory threshold for {}",
                input.output_amount.asset
            ),
        );
    };
    if remaining_output < threshold {
        return rejected(
            RejectionReason::InventoryBelowQuoteableThreshold,
            format!(
                "remaining {} inventory {} would fall below quoteable threshold {}",
                input.output_amount.asset,
                remaining_output.as_u64(),
                threshold.as_u64()
            ),
        );
    }
    checks.push("quoteable inventory".to_owned());

    if let Some(decision) = sol_gas_decision(input, policy) {
        return decision;
    }
    checks.push("SOL gas buffer".to_owned());

    RiskDecision::Accepted { checks }
}

fn rejected(reason: RejectionReason, detail: String) -> RiskDecision {
    RiskDecision::Rejected {
        reason,
        details: vec![detail],
    }
}

fn threshold_for(policy: &RiskPolicy, asset: &AssetId) -> Option<AmountRaw> {
    policy
        .quoteable_thresholds
        .iter()
        .find(|amount| &amount.asset == asset)
        .map(|amount| amount.amount_raw)
}

fn exposure_limit_for(policy: &RiskPolicy, asset: &AssetId) -> Option<Decimal> {
    policy
        .exposure_limits_usd
        .iter()
        .find(|(candidate, _)| candidate == asset)
        .map(|(_, limit)| *limit)
}

fn current_exposure_for(policy: &RiskPolicy, asset: &AssetId) -> Decimal {
    policy
        .current_exposure_usd
        .iter()
        .find(|(candidate, _)| candidate == asset)
        .map_or(Decimal::ZERO, |(_, exposure)| *exposure)
}

fn exposure_decision(input: &RiskEvaluationInput, policy: &RiskPolicy) -> Option<RiskDecision> {
    for (asset, delta) in [
        (&input.input_amount.asset, input.notional_usd),
        (&input.output_amount.asset, -input.notional_usd),
    ] {
        let Some(limit) = exposure_limit_for(policy, asset) else {
            return Some(rejected(
                RejectionReason::ValidationFailed,
                format!("missing exposure limit for {asset}"),
            ));
        };
        if limit <= Decimal::ZERO {
            return Some(rejected(
                RejectionReason::ValidationFailed,
                format!("exposure limit for {asset} must be positive"),
            ));
        }

        let projected = current_exposure_for(policy, asset) + delta;
        if projected.abs() > limit {
            return Some(rejected(
                RejectionReason::ExposureLimitExceeded,
                format!("projected {asset} exposure {projected} exceeds limit {limit}"),
            ));
        }
    }

    None
}

fn sol_gas_decision(input: &RiskEvaluationInput, policy: &RiskPolicy) -> Option<RiskDecision> {
    if policy.native_sol_gas_buffer_raw.is_zero() {
        return None;
    }

    let sol = AssetId::from("SOL");
    let sol_balance = input.inventory.balance_for(&sol).as_u64();
    let spend = if input.output_amount.asset == sol {
        input.output_amount.amount_raw.as_u64()
    } else {
        0
    };
    let remaining = sol_balance.saturating_sub(spend);

    if remaining < policy.native_sol_gas_buffer_raw.as_u64() {
        return Some(rejected(
            RejectionReason::InventoryBelowQuoteableThreshold,
            format!(
                "SOL gas buffer would be {} lamports after quote; required {}",
                remaining,
                policy.native_sol_gas_buffer_raw.as_u64()
            ),
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AmountRaw;

    fn usdc() -> AssetId {
        AssetId::from("USDC")
    }

    fn sol() -> AssetId {
        AssetId::from("SOL")
    }

    fn taker() -> WalletAddress {
        WalletAddress::new("taker111111111111111111111111111111111111111")
    }

    fn other_taker() -> WalletAddress {
        WalletAddress::new("other111111111111111111111111111111111111111")
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_000)
    }

    fn inventory(sol_balance: u64) -> InventorySnapshot {
        InventorySnapshot {
            balances: vec![
                TokenAmount::new(usdc(), AmountRaw::new(10_000_000)),
                TokenAmount::new(sol(), AmountRaw::new(sol_balance)),
            ],
            targets: vec![
                TokenAmount::new(AssetId::from("USDC"), AmountRaw::new(10_000_000)),
                TokenAmount::new(AssetId::from("SOL"), AmountRaw::new(1_000_000_000)),
            ],
            observed_at: now(),
        }
    }

    fn policy() -> RiskPolicy {
        RiskPolicy {
            supported_pairs: vec![AssetPair::new(usdc(), sol())],
            allowlisted_takers: vec![taker()],
            require_taker_allowlist: false,
            max_quote_notional_usd: Some(Decimal::new(2, 0)),
            max_price_staleness_seconds: 20,
            quoteable_thresholds: vec![
                TokenAmount::new(usdc(), AmountRaw::new(1_000_000)),
                TokenAmount::new(sol(), AmountRaw::new(1_000_000)),
            ],
            exposure_limits_usd: vec![(usdc(), Decimal::new(10, 0)), (sol(), Decimal::new(10, 0))],
            current_exposure_usd: vec![(usdc(), Decimal::ZERO), (sol(), Decimal::ZERO)],
            native_sol_gas_buffer_raw: AmountRaw::new(20_000_000),
            asset_usd_prices: vec![(usdc(), Decimal::ONE)],
        }
    }

    fn input() -> RiskEvaluationInput {
        RiskEvaluationInput {
            pair: AssetPair::new(usdc(), sol()),
            taker_wallet: taker(),
            input_amount: TokenAmount::new(usdc(), AmountRaw::new(1_000_000)),
            output_amount: TokenAmount::new(sol(), AmountRaw::new(9_965_000)),
            reference_price: ReferencePrice {
                pair: AssetPair::new(usdc(), sol()),
                output_per_input: Decimal::new(1, 2),
                observed_at: now(),
            },
            notional_usd: Decimal::ONE,
            now: now(),
            expires_at: now() + time::Duration::seconds(45),
            inventory: inventory(1_000_000_000),
        }
    }

    fn rejection_reason(decision: &RiskDecision) -> Option<&RejectionReason> {
        match decision {
            RiskDecision::Rejected { reason, .. } => Some(reason),
            RiskDecision::Accepted { .. } => None,
        }
    }

    #[test]
    fn risk_accepts_valid_inventory_first_quote() {
        assert!(matches!(
            evaluate_pre_quote(&input(), &policy()),
            RiskDecision::Accepted { .. }
        ));
    }

    #[test]
    fn risk_rejects_oversized_quote() {
        let mut input = input();
        input.notional_usd = Decimal::new(3, 0);

        let decision = evaluate_pre_quote(&input, &policy());

        assert_eq!(
            rejection_reason(&decision),
            Some(&RejectionReason::MaxNotionalExceeded)
        );
    }

    #[test]
    fn risk_rejects_unsupported_pair() {
        let mut input = input();
        input.pair = AssetPair::new(sol(), usdc());

        let decision = evaluate_pre_quote(&input, &policy());

        assert_eq!(
            rejection_reason(&decision),
            Some(&RejectionReason::UnsupportedPair)
        );
    }

    #[test]
    fn risk_rejects_stale_price() {
        let mut input = input();
        input.reference_price.observed_at = now() - time::Duration::seconds(21);

        let decision = evaluate_pre_quote(&input, &policy());

        assert_eq!(
            rejection_reason(&decision),
            Some(&RejectionReason::StalePrice)
        );
    }

    #[test]
    fn risk_rejects_missing_risk_limit() {
        let mut policy = policy();
        policy
            .exposure_limits_usd
            .retain(|(asset, _)| asset != &sol());

        let decision = evaluate_pre_quote(&input(), &policy);

        assert_eq!(
            rejection_reason(&decision),
            Some(&RejectionReason::ValidationFailed)
        );
    }

    #[test]
    fn risk_rejects_insufficient_inventory() {
        let mut input = input();
        input.inventory = inventory(5_000_000);

        let decision = evaluate_pre_quote(&input, &policy());

        assert_eq!(
            rejection_reason(&decision),
            Some(&RejectionReason::InventoryBelowQuoteableThreshold)
        );
    }

    #[test]
    fn risk_protects_sol_gas_buffer() {
        let mut input = input();
        input.inventory = inventory(25_000_000);

        let decision = evaluate_pre_quote(&input, &policy());

        assert_eq!(
            rejection_reason(&decision),
            Some(&RejectionReason::InventoryBelowQuoteableThreshold)
        );
        assert!(matches!(
            decision,
            RiskDecision::Rejected { details, .. } if details.iter().any(|detail| detail.contains("SOL gas buffer"))
        ));
    }

    #[test]
    fn risk_rejects_wallet_not_on_allowlist() {
        let mut input = input();
        input.taker_wallet = other_taker();
        let mut policy = policy();
        policy.require_taker_allowlist = true;

        let decision = evaluate_pre_quote(&input, &policy);

        assert_eq!(
            rejection_reason(&decision),
            Some(&RejectionReason::WalletNotAllowed)
        );
    }
}
