//! Deterministic RFQ quote math.

use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::domain::types::{AmountRaw, AssetId, AssetPair, ReferencePrice, TokenAmount};

/// Inventory balances used by quote math and pre-quote risk checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventorySnapshot {
    /// Working wallet balances by asset.
    pub balances: Vec<TokenAmount>,
    /// Target working balances by asset, used to skew spreads around inventory.
    pub targets: Vec<TokenAmount>,
    /// Snapshot timestamp.
    pub observed_at: OffsetDateTime,
}

impl InventorySnapshot {
    /// Return the working balance for an asset, or zero when absent.
    #[must_use]
    pub fn balance_for(&self, asset: &AssetId) -> AmountRaw {
        self.balances
            .iter()
            .find(|amount| &amount.asset == asset)
            .map(|amount| amount.amount_raw)
            .unwrap_or_default()
    }

    /// Return the target balance for an asset, or zero when absent.
    #[must_use]
    pub fn target_for(&self, asset: &AssetId) -> AmountRaw {
        self.targets
            .iter()
            .find(|amount| &amount.asset == asset)
            .map(|amount| amount.amount_raw)
            .unwrap_or_default()
    }
}

/// Configurable deterministic quote math knobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteEngineConfig {
    /// Base maker spread in basis points.
    pub base_spread_bps: i32,
    /// Fee allowance in basis points.
    pub fee_bps: i32,
    /// Minimum maker edge after all adjustments.
    pub min_profit_bps: i32,
    /// Maximum absolute inventory skew adjustment in basis points.
    pub max_inventory_skew_bps: i32,
}

impl Default for QuoteEngineConfig {
    fn default() -> Self {
        Self {
            base_spread_bps: 35,
            fee_bps: 5,
            min_profit_bps: 20,
            max_inventory_skew_bps: 50,
        }
    }
}

/// Audit-friendly quote math breakdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteBreakdown {
    /// Raw output before maker spread and fee adjustments.
    pub reference_output_amount_raw: AmountRaw,
    /// Base spread in basis points.
    pub base_spread_bps: i32,
    /// Inventory skew in basis points.
    pub inventory_skew_bps: i32,
    /// Fee allowance in basis points.
    pub fee_bps: i32,
    /// Minimum profit floor in basis points.
    pub min_profit_bps: i32,
    /// Final spread applied to output.
    pub total_spread_bps: i32,
    /// Reference price used for the quote.
    pub reference_price: ReferencePrice,
}

/// Result of deterministic quote math.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteComputation {
    /// Directional pair.
    pub pair: AssetPair,
    /// Input amount supplied by the taker.
    pub input_amount: TokenAmount,
    /// Output amount the maker is willing to pay.
    pub output_amount: TokenAmount,
    /// Full spread and reference-price breakdown.
    pub breakdown: QuoteBreakdown,
}

/// Quote math failures mapped by the RFQ layer into structured rejections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QuoteMathError {
    /// The input amount was zero.
    #[error("input amount must be greater than zero")]
    AmountTooSmall,
    /// The reference price was zero or negative.
    #[error("reference price must be positive")]
    InvalidReferencePrice,
    /// Decimal conversion overflowed raw token units.
    #[error("amount conversion overflowed raw token units")]
    AmountOverflow,
    /// Spread would consume the entire output side.
    #[error("total spread must be below 10000 bps")]
    SpreadTooWide,
}

/// Calculate a deterministic inventory-first quote.
///
/// # Errors
///
/// Returns a quote math error when amounts, prices, or spread values are outside
/// safe raw-token boundaries.
pub fn calculate_quote(
    pair: AssetPair,
    input_amount_raw: AmountRaw,
    reference_price: ReferencePrice,
    input_decimals: u8,
    output_decimals: u8,
    inventory: &InventorySnapshot,
    config: &QuoteEngineConfig,
) -> Result<QuoteComputation, QuoteMathError> {
    if input_amount_raw.is_zero() {
        return Err(QuoteMathError::AmountTooSmall);
    }

    if reference_price.pair != pair || reference_price.output_per_input <= Decimal::ZERO {
        return Err(QuoteMathError::InvalidReferencePrice);
    }

    let input_ui = raw_to_decimal(input_amount_raw, input_decimals)?;
    let reference_output_ui = input_ui * reference_price.output_per_input;
    let reference_output_amount_raw = decimal_to_raw_floor(reference_output_ui, output_decimals)?;
    if reference_output_amount_raw.is_zero() {
        return Err(QuoteMathError::AmountTooSmall);
    }

    let inventory_skew_bps = inventory_skew_bps(&pair, inventory, config);
    let total_before_floor = config.base_spread_bps + config.fee_bps + inventory_skew_bps;
    let total_spread_bps = total_before_floor.max(config.min_profit_bps).max(0);
    if total_spread_bps >= 10_000 {
        return Err(QuoteMathError::SpreadTooWide);
    }

    let retained_bps = Decimal::from(10_000 - total_spread_bps) / Decimal::from(10_000);
    let output_amount_raw = decimal_to_raw_floor(
        Decimal::from(reference_output_amount_raw.as_u64()) * retained_bps,
        0,
    )?;
    if output_amount_raw.is_zero() {
        return Err(QuoteMathError::AmountTooSmall);
    }

    Ok(QuoteComputation {
        pair: pair.clone(),
        input_amount: TokenAmount::new(pair.input, input_amount_raw),
        output_amount: TokenAmount::new(pair.output, output_amount_raw),
        breakdown: QuoteBreakdown {
            reference_output_amount_raw,
            base_spread_bps: config.base_spread_bps,
            inventory_skew_bps,
            fee_bps: config.fee_bps,
            min_profit_bps: config.min_profit_bps,
            total_spread_bps,
            reference_price,
        },
    })
}

fn raw_to_decimal(amount: AmountRaw, decimals: u8) -> Result<Decimal, QuoteMathError> {
    Ok(Decimal::from(amount.as_u64()) / decimal_factor(decimals)?)
}

fn decimal_to_raw_floor(value: Decimal, decimals: u8) -> Result<AmountRaw, QuoteMathError> {
    if value.is_sign_negative() {
        return Err(QuoteMathError::AmountTooSmall);
    }

    let scaled = (value * decimal_factor(decimals)?).trunc();
    let raw = scaled.to_u64().ok_or(QuoteMathError::AmountOverflow)?;
    Ok(AmountRaw::new(raw))
}

fn decimal_factor(decimals: u8) -> Result<Decimal, QuoteMathError> {
    let mut factor = 1_u64;
    for _ in 0..decimals {
        factor = factor
            .checked_mul(10)
            .ok_or(QuoteMathError::AmountOverflow)?;
    }
    Ok(Decimal::from(factor))
}

fn inventory_skew_bps(
    pair: &AssetPair,
    inventory: &InventorySnapshot,
    config: &QuoteEngineConfig,
) -> i32 {
    if config.max_inventory_skew_bps <= 0 {
        return 0;
    }

    let max_skew = Decimal::from(config.max_inventory_skew_bps);
    let source_skew = side_skew(
        inventory.balance_for(&pair.input),
        inventory.target_for(&pair.input),
        max_skew,
        false,
    );
    let output_skew = side_skew(
        inventory.balance_for(&pair.output),
        inventory.target_for(&pair.output),
        max_skew,
        true,
    );
    let combined = (source_skew + output_skew).clamp(-max_skew, max_skew);
    combined.round().to_i32().unwrap_or_else(|| {
        if combined.is_sign_negative() {
            -config.max_inventory_skew_bps
        } else {
            config.max_inventory_skew_bps
        }
    })
}

fn side_skew(
    balance: AmountRaw,
    target: AmountRaw,
    max_skew: Decimal,
    underweight_widens: bool,
) -> Decimal {
    if target.is_zero() {
        return Decimal::ZERO;
    }

    let balance = Decimal::from(balance.as_u64());
    let target = Decimal::from(target.as_u64());
    let ratio_delta = if underweight_widens {
        (target - balance) / target
    } else {
        (balance - target) / target
    };

    ratio_delta * max_skew
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    fn usdc() -> AssetId {
        AssetId::from("USDC")
    }

    fn sol() -> AssetId {
        AssetId::from("SOL")
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_000)
    }

    fn inventory(sol_balance: u64, sol_target: u64) -> InventorySnapshot {
        InventorySnapshot {
            balances: vec![
                TokenAmount::new(usdc(), AmountRaw::new(10_000_000)),
                TokenAmount::new(sol(), AmountRaw::new(sol_balance)),
            ],
            targets: vec![
                TokenAmount::new(AssetId::from("USDC"), AmountRaw::new(10_000_000)),
                TokenAmount::new(AssetId::from("SOL"), AmountRaw::new(sol_target)),
            ],
            observed_at: now(),
        }
    }

    fn reference_price() -> ReferencePrice {
        ReferencePrice {
            pair: AssetPair::new(usdc(), sol()),
            output_per_input: Decimal::new(1, 2),
            observed_at: now(),
        }
    }

    #[test]
    fn quote_engine_applies_base_spread_fees_and_min_profit() {
        let config = QuoteEngineConfig {
            base_spread_bps: 30,
            fee_bps: 5,
            min_profit_bps: 20,
            max_inventory_skew_bps: 50,
        };

        let quote = calculate_quote(
            AssetPair::new(usdc(), sol()),
            AmountRaw::new(1_000_000),
            reference_price(),
            6,
            9,
            &inventory(1_000_000_000, 1_000_000_000),
            &config,
        )
        .expect("quote should calculate");

        assert_eq!(
            quote.breakdown.reference_output_amount_raw,
            AmountRaw::new(10_000_000)
        );
        assert_eq!(quote.breakdown.base_spread_bps, 30);
        assert_eq!(quote.breakdown.fee_bps, 5);
        assert_eq!(quote.breakdown.inventory_skew_bps, 0);
        assert_eq!(quote.breakdown.total_spread_bps, 35);
        assert_eq!(quote.output_amount.amount_raw, AmountRaw::new(9_965_000));
    }

    #[test]
    fn quote_engine_widens_when_output_inventory_is_under_target() {
        let config = QuoteEngineConfig {
            base_spread_bps: 30,
            fee_bps: 5,
            min_profit_bps: 20,
            max_inventory_skew_bps: 50,
        };

        let quote = calculate_quote(
            AssetPair::new(usdc(), sol()),
            AmountRaw::new(1_000_000),
            reference_price(),
            6,
            9,
            &inventory(500_000_000, 1_000_000_000),
            &config,
        )
        .expect("quote should calculate");

        assert!(quote.breakdown.inventory_skew_bps > 0);
        assert_eq!(quote.breakdown.total_spread_bps, 60);
        assert_eq!(quote.output_amount.amount_raw, AmountRaw::new(9_940_000));
    }
}
