//! Inventory valuation, quoteability, and drift calculations.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use time::OffsetDateTime;

use crate::config::AppConfig;
use crate::error::AppResult;
use crate::events::{EventMetadata, InventoryEvent};
use crate::types::{
    AmountRaw, AssetId, BalanceSnapshot, RejectionReason, RuntimeRunId, TokenAmount, WalletRole,
};

/// Default hard reserve retained for Solana transaction fees.
pub const DEFAULT_NATIVE_SOL_GAS_BUFFER_RAW: u64 = 10_000_000;

/// Local inventory policy derived from config, with test-friendly overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryPolicy {
    /// SOL raw units retained for gas and never spent by automated swaps.
    pub native_sol_gas_buffer_raw: AmountRaw,
}

impl InventoryPolicy {
    /// Build inventory policy from application config.
    #[must_use]
    pub fn from_config(config: &AppConfig) -> Self {
        let sol_buffer = config
            .assets
            .supported
            .iter()
            .find(|asset| asset.id.as_str() == "SOL")
            .map_or(AmountRaw::new(DEFAULT_NATIVE_SOL_GAS_BUFFER_RAW), |asset| {
                asset.quoteable_threshold_raw
            });

        Self {
            native_sol_gas_buffer_raw: sol_buffer,
        }
    }
}

impl Default for InventoryPolicy {
    fn default() -> Self {
        Self {
            native_sol_gas_buffer_raw: AmountRaw::new(DEFAULT_NATIVE_SOL_GAS_BUFFER_RAW),
        }
    }
}

/// One asset's current working-inventory projection.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryPosition {
    /// Asset being tracked.
    pub asset: AssetId,
    /// Token decimals from config.
    pub decimals: u8,
    /// Current raw working balance.
    pub balance_raw: AmountRaw,
    /// Current UI-unit balance.
    pub balance_units: Decimal,
    /// Reference USD price per UI unit.
    pub price_usd: Decimal,
    /// Current USD value.
    pub value_usd: Decimal,
    /// Target allocation weight.
    pub target_weight: Decimal,
    /// Target USD value at the current total inventory value.
    pub target_value_usd: Decimal,
    /// Target raw token balance at the current reference price.
    pub target_balance_raw: AmountRaw,
    /// Configured raw quoteability threshold.
    pub quoteable_threshold_raw: AmountRaw,
    /// Whether the current balance can be quoted.
    pub quoteable: bool,
    /// Signed drift versus target in basis points.
    pub drift_bps: i32,
}

impl InventoryPosition {
    /// Absolute USD excess above target.
    #[must_use]
    pub fn excess_value_usd(&self) -> Decimal {
        (self.value_usd - self.target_value_usd).max(Decimal::ZERO)
    }

    /// Absolute USD deficit below target.
    #[must_use]
    pub fn deficit_value_usd(&self) -> Decimal {
        (self.target_value_usd - self.value_usd).max(Decimal::ZERO)
    }
}

/// Point-in-time working inventory snapshot for a wallet.
#[derive(Debug, Clone, PartialEq)]
pub struct InventorySnapshot {
    /// Wallet represented by this snapshot.
    pub wallet: WalletRole,
    /// Snapshot timestamp.
    pub observed_at: OffsetDateTime,
    /// Per-asset positions keyed by asset id.
    pub positions: BTreeMap<AssetId, InventoryPosition>,
    /// Total USD value across priced assets.
    pub total_value_usd: Decimal,
    /// Largest absolute drift in basis points.
    pub max_abs_drift_bps: i32,
}

impl InventorySnapshot {
    /// Build a deterministic inventory snapshot from a balance reader result.
    ///
    /// # Errors
    ///
    /// This currently has no fallible adapter work, but returns `AppResult` so
    /// future validation can be added without changing callers.
    pub fn from_balance_snapshot(
        config: &AppConfig,
        balance_snapshot: BalanceSnapshot,
        prices_usd: &BTreeMap<AssetId, Decimal>,
        _policy: &InventoryPolicy,
    ) -> AppResult<Self> {
        let raw_balances: BTreeMap<AssetId, AmountRaw> = balance_snapshot
            .balances
            .into_iter()
            .map(|balance| (balance.asset, balance.amount_raw))
            .collect();

        let total_value_usd = config
            .assets
            .supported
            .iter()
            .filter(|asset| asset.enabled)
            .map(|asset| {
                let raw = raw_balances
                    .get(&asset.id)
                    .copied()
                    .unwrap_or_else(|| AmountRaw::new(0));
                raw_to_units(raw, asset.decimals) * price_for(&asset.id, prices_usd)
            })
            .sum();

        let mut positions = BTreeMap::new();
        let mut max_abs_drift_bps = 0_i32;

        for asset in config.assets.supported.iter().filter(|asset| asset.enabled) {
            let balance_raw = raw_balances
                .get(&asset.id)
                .copied()
                .unwrap_or_else(|| AmountRaw::new(0));
            let balance_units = raw_to_units(balance_raw, asset.decimals);
            let usd_price = price_for(&asset.id, prices_usd);
            let value_usd = balance_units * usd_price;
            let target_value_usd = total_value_usd * asset.target_weight;
            let target_balance_raw = if usd_price > Decimal::ZERO {
                units_to_raw_floor(target_value_usd / usd_price, asset.decimals)
            } else {
                AmountRaw::new(0)
            };
            let drift_bps = drift_bps(value_usd, target_value_usd);
            max_abs_drift_bps = max_abs_drift_bps.max(drift_bps.saturating_abs());

            positions.insert(
                asset.id.clone(),
                InventoryPosition {
                    asset: asset.id.clone(),
                    decimals: asset.decimals,
                    balance_raw,
                    balance_units,
                    price_usd: usd_price,
                    value_usd,
                    target_weight: asset.target_weight,
                    target_value_usd,
                    target_balance_raw,
                    quoteable_threshold_raw: asset.quoteable_threshold_raw,
                    quoteable: balance_raw.as_u64() >= asset.quoteable_threshold_raw.as_u64(),
                    drift_bps,
                },
            );
        }

        Ok(Self {
            wallet: balance_snapshot.wallet,
            observed_at: balance_snapshot.observed_at,
            positions,
            total_value_usd,
            max_abs_drift_bps,
        })
    }

    /// Return a position by asset id.
    #[must_use]
    pub fn position(&self, asset: &AssetId) -> Option<&InventoryPosition> {
        self.positions.get(asset)
    }

    /// Return raw balance for an asset, defaulting to zero when absent.
    #[must_use]
    pub fn raw_balance(&self, asset: &AssetId) -> AmountRaw {
        self.position(asset)
            .map_or(AmountRaw::new(0), |position| position.balance_raw)
    }

    /// Return whether this asset has enough working balance to quote.
    #[must_use]
    pub fn is_quoteable(&self, asset: &AssetId) -> bool {
        self.position(asset)
            .is_some_and(|position| position.quoteable)
    }

    /// Return the raw balance available as a swap source after gas protection.
    #[must_use]
    pub fn source_available_raw(&self, asset: &AssetId, policy: &InventoryPolicy) -> AmountRaw {
        if asset.as_str() == "SOL" {
            return AmountRaw::new(
                self.raw_balance(asset)
                    .as_u64()
                    .saturating_sub(policy.native_sol_gas_buffer_raw.as_u64()),
            );
        }

        self.raw_balance(asset)
    }

    /// True when the wallet has the configured native SOL gas buffer.
    #[must_use]
    pub fn has_native_sol_gas_buffer(&self, policy: &InventoryPolicy) -> bool {
        self.raw_balance(&AssetId::from("SOL")).as_u64()
            >= policy.native_sol_gas_buffer_raw.as_u64()
    }

    /// Return inventory projection events for breached thresholds and drift.
    #[must_use]
    pub fn inventory_events(
        &self,
        run_id: RuntimeRunId,
        drift_threshold_bps: i32,
    ) -> Vec<InventoryEvent> {
        let mut events = Vec::new();
        for position in self.positions.values() {
            if !position.quoteable {
                events.push(InventoryEvent::ThresholdBreached {
                    metadata: EventMetadata::new(run_id),
                    asset: TokenAmount::new(position.asset.clone(), position.balance_raw),
                    reason: RejectionReason::InventoryBelowQuoteableThreshold,
                });
            }

            if drift_threshold_bps > 0 && position.drift_bps.saturating_abs() >= drift_threshold_bps
            {
                events.push(InventoryEvent::DriftDetected {
                    metadata: EventMetadata::new(run_id),
                    asset: TokenAmount::new(position.asset.clone(), position.balance_raw),
                    drift_bps: position.drift_bps,
                });
            }
        }
        events
    }
}

/// Convert raw token units into UI units.
#[must_use]
pub fn raw_to_units(amount: AmountRaw, decimals: u8) -> Decimal {
    Decimal::from(amount.as_u64()) / decimal_pow10(decimals)
}

/// Convert UI units into raw token units, rounding down.
#[must_use]
pub fn units_to_raw_floor(units: Decimal, decimals: u8) -> AmountRaw {
    if units <= Decimal::ZERO {
        return AmountRaw::new(0);
    }

    AmountRaw::new(
        (units * decimal_pow10(decimals))
            .trunc()
            .to_u64()
            .unwrap_or(u64::MAX),
    )
}

fn price_for(asset: &AssetId, prices_usd: &BTreeMap<AssetId, Decimal>) -> Decimal {
    prices_usd.get(asset).copied().unwrap_or_else(|| {
        if asset.as_str() == "USDC" {
            Decimal::ONE
        } else {
            Decimal::ZERO
        }
    })
}

fn decimal_pow10(decimals: u8) -> Decimal {
    Decimal::from(10_u64.pow(u32::from(decimals)))
}

fn drift_bps(value_usd: Decimal, target_value_usd: Decimal) -> i32 {
    if target_value_usd == Decimal::ZERO {
        return if value_usd > Decimal::ZERO {
            i32::MAX
        } else {
            0
        };
    }

    decimal_to_i32_saturating(
        ((value_usd - target_value_usd) / target_value_usd) * Decimal::from(10_000),
    )
}

fn decimal_to_i32_saturating(value: Decimal) -> i32 {
    value.round_dp(0).to_i32().unwrap_or_else(|| {
        if value.is_sign_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn inventory_target_allocation_and_drift_are_calculated_from_reference_prices() {
        let config = AppConfig::default();
        let snapshot = InventorySnapshot::from_balance_snapshot(
            &config,
            balance_snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(6_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(40_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &prices(),
            &InventoryPolicy::from_config(&config),
        )
        .expect("snapshot");

        let sol = snapshot.position(&asset("SOL")).expect("sol position");
        assert_eq!(snapshot.total_value_usd, Decimal::from(10));
        assert_eq!(sol.target_value_usd, Decimal::new(35, 1));
        assert_eq!(sol.target_balance_raw, AmountRaw::new(35_000_000));
        assert_eq!(sol.drift_bps, 1_429);
    }

    #[test]
    fn inventory_quoteable_thresholds_mark_low_balances_as_not_quoteable() {
        let config = AppConfig::default();
        let snapshot = InventorySnapshot::from_balance_snapshot(
            &config,
            balance_snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(6_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(5_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(2_000)),
            ]),
            &prices(),
            &InventoryPolicy::from_config(&config),
        )
        .expect("snapshot");

        assert!(!snapshot.is_quoteable(&asset("SOL")));
        assert!(snapshot.is_quoteable(&asset("USDC")));
        assert_eq!(
            snapshot
                .position(&asset("SOL"))
                .expect("sol position")
                .quoteable_threshold_raw,
            AmountRaw::new(10_000_000)
        );
    }

    #[test]
    fn inventory_source_available_raw_protects_native_sol_gas_buffer() {
        let snapshot = InventorySnapshot {
            wallet: WalletRole::Maker,
            observed_at: OffsetDateTime::UNIX_EPOCH,
            positions: BTreeMap::from([(
                asset("SOL"),
                InventoryPosition {
                    asset: asset("SOL"),
                    decimals: 9,
                    balance_raw: AmountRaw::new(15_000_000),
                    balance_units: Decimal::new(15, 3),
                    price_usd: Decimal::from(100),
                    value_usd: Decimal::new(15, 1),
                    target_weight: Decimal::ZERO,
                    target_value_usd: Decimal::ZERO,
                    target_balance_raw: AmountRaw::new(0),
                    quoteable_threshold_raw: AmountRaw::new(10_000_000),
                    quoteable: true,
                    drift_bps: 0,
                },
            )]),
            total_value_usd: Decimal::new(15, 1),
            max_abs_drift_bps: 0,
        };

        assert_eq!(
            snapshot.source_available_raw(&asset("SOL"), &InventoryPolicy::default()),
            AmountRaw::new(5_000_000)
        );
    }

    #[test]
    fn inventory_events_include_threshold_breaches() {
        let config = AppConfig::default();
        let snapshot = InventorySnapshot::from_balance_snapshot(
            &config,
            balance_snapshot(vec![
                TokenAmount::new(asset("USDC"), AmountRaw::new(1_000_000)),
                TokenAmount::new(asset("SOL"), AmountRaw::new(5_000_000)),
                TokenAmount::new(asset("cbBTC"), AmountRaw::new(0)),
            ]),
            &prices(),
            &InventoryPolicy::from_config(&config),
        )
        .expect("snapshot");

        let events = snapshot.inventory_events(RuntimeRunId::generate(), 1_000);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                InventoryEvent::ThresholdBreached {
                    reason: RejectionReason::InventoryBelowQuoteableThreshold,
                    ..
                }
            )
        }));
    }
}
