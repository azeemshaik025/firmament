//! USDC-estimated P&L projection derived from runtime events and ledger costs.

use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use rusqlite::{OptionalExtension, params};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::adapters::persistence::db::{Db, sqlite_error};
use crate::domain::assets::{AssetError, AssetRegistry, USDC_ID};
use crate::domain::events::{RuntimeEvent, SwapEvent};
use crate::domain::types::{AssetId, AssetPair, ReferencePrice, TokenAmount};
use crate::error::AppError;

/// P&L estimate category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PnlCategory {
    /// Realized RFQ/trading spread.
    RealizedSpread,
    /// Explicit operational fee.
    Fees,
    /// Jupiter rebalance cost.
    RebalanceCost,
    /// Hedge execution or carry cost.
    HedgeCost,
}

impl PnlCategory {
    #[must_use]
    const fn as_str(self) -> &'static str {
        match self {
            Self::RealizedSpread => "realized_spread",
            Self::Fees => "fees",
            Self::RebalanceCost => "rebalance_cost",
            Self::HedgeCost => "hedge_cost",
        }
    }
}

impl Display for PnlCategory {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for PnlCategory {
    type Error = AppError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "realized_spread" => Ok(Self::RealizedSpread),
            "fees" => Ok(Self::Fees),
            "rebalance_cost" => Ok(Self::RebalanceCost),
            "hedge_cost" => Ok(Self::HedgeCost),
            _ => Err(AppError::persistence(format!(
                "invalid P&L category: {value}"
            ))),
        }
    }
}

/// Persisted P&L estimate in USDC terms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PnlEstimate {
    /// Estimate UUID.
    pub id: Uuid,
    /// Source event UUID.
    pub source_event_id: Uuid,
    /// Related reference type.
    pub reference_type: String,
    /// Related reference UUID.
    pub reference_id: Uuid,
    /// Estimate category.
    pub category: PnlCategory,
    /// Token asset being valued.
    pub asset: AssetId,
    /// Token amount in signed raw units.
    pub amount_raw: i128,
    /// Signed USDC value. Costs are negative.
    pub usdc_value: Decimal,
    /// Description for operator views.
    pub description: String,
    /// Creation timestamp.
    pub created_at: OffsetDateTime,
}

impl PnlEstimate {
    /// Build a deterministic estimate value for tests and consumers.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_event_id: Uuid,
        reference_type: impl Into<String>,
        reference_id: Uuid,
        category: PnlCategory,
        amount: &TokenAmount,
        usdc_value: Decimal,
        description: impl Into<String>,
        created_at: OffsetDateTime,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            source_event_id,
            reference_type: reference_type.into(),
            reference_id,
            category,
            asset: amount.asset.clone(),
            amount_raw: i128::from(amount.amount_raw.as_u64()),
            usdc_value,
            description: description.into(),
            created_at,
        }
    }
}

/// P&L calculation failures.
#[derive(Debug, Error)]
pub enum PnlError {
    /// Asset metadata was unavailable.
    #[error(transparent)]
    Asset(#[from] AssetError),
    /// No USDC conversion was known for an asset.
    #[error("missing USDC price for asset {0}")]
    MissingUsdcPrice(AssetId),
    /// Price was zero.
    #[error("zero price for pair {0:?}")]
    ZeroPrice(AssetPair),
    /// Decimal math overflowed.
    #[error("decimal overflow estimating USDC value")]
    DecimalOverflow,
}

impl From<PnlError> for AppError {
    fn from(error: PnlError) -> Self {
        match error {
            PnlError::Asset(asset_error) => asset_error.into(),
            PnlError::MissingUsdcPrice(_) | PnlError::ZeroPrice(_) | PnlError::DecimalOverflow => {
                Self::validation(error.to_string())
            }
        }
    }
}

/// In-memory reference price book for USDC estimates.
#[derive(Debug, Clone, Default)]
pub struct PnlPriceBook {
    prices: BTreeMap<(AssetId, AssetId), Decimal>,
}

impl PnlPriceBook {
    /// Record a price observation.
    pub fn record_price(&mut self, price: &ReferencePrice) {
        self.prices.insert(
            (price.pair.input.clone(), price.pair.output.clone()),
            price.output_per_input,
        );
    }

    /// Estimate a token amount in USDC display units.
    ///
    /// # Errors
    ///
    /// Returns an error when metadata or conversion price is missing.
    pub fn estimate_usdc(
        &self,
        registry: &AssetRegistry,
        amount: &TokenAmount,
    ) -> Result<Decimal, PnlError> {
        let display_amount = registry.raw_to_display(&amount.asset, amount.amount_raw)?;
        let usdc = AssetId::from(USDC_ID);
        if amount.asset == usdc {
            return Ok(display_amount);
        }

        if let Some(price) = self.prices.get(&(amount.asset.clone(), usdc.clone())) {
            return display_amount
                .checked_mul(*price)
                .ok_or(PnlError::DecimalOverflow);
        }

        if let Some(inverse_price) = self.prices.get(&(usdc, amount.asset.clone())) {
            if inverse_price.is_zero() {
                return Err(PnlError::ZeroPrice(AssetPair::new(
                    AssetId::from(USDC_ID),
                    amount.asset.clone(),
                )));
            }
            return display_amount
                .checked_div(*inverse_price)
                .ok_or(PnlError::DecimalOverflow);
        }

        Err(PnlError::MissingUsdcPrice(amount.asset.clone()))
    }
}

/// Aggregated P&L summary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PnlSummary {
    /// Realized spread in USDC.
    pub realized_spread_usdc: Decimal,
    /// Fees in USDC, normally negative.
    pub fees_usdc: Decimal,
    /// Rebalance costs in USDC, normally negative.
    pub rebalance_cost_usdc: Decimal,
    /// Hedge costs in USDC, normally negative.
    pub hedge_cost_usdc: Decimal,
    /// Net total in USDC.
    pub net_usdc: Decimal,
}

/// `SQLite`-backed P&L estimate repository.
pub struct PnlRepository<'db> {
    db: &'db Db,
}

impl<'db> PnlRepository<'db> {
    /// Create a P&L repository.
    #[must_use]
    pub const fn new(db: &'db Db) -> Self {
        Self { db }
    }

    /// Persist an estimate idempotently.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when `SQLite` rejects the write.
    pub fn save_estimate(&self, estimate: &PnlEstimate) -> Result<bool, AppError> {
        self.db.with_connection(|connection| {
            let existing = connection
                .query_row(
                    "SELECT id FROM pnl_estimates
                     WHERE source_event_id = ?1 AND category = ?2 AND asset_id = ?3",
                    params![
                        estimate.source_event_id.to_string(),
                        estimate.category.to_string(),
                        estimate.asset.as_str(),
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            if existing.is_some() {
                return Ok(false);
            }

            let rows = connection
                .execute(
                    "INSERT INTO pnl_estimates
                        (id, source_event_id, reference_type, reference_id, category,
                         asset_id, amount_raw, usdc_value, description, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        estimate.id.to_string(),
                        estimate.source_event_id.to_string(),
                        estimate.reference_type.as_str(),
                        estimate.reference_id.to_string(),
                        estimate.category.to_string(),
                        estimate.asset.as_str(),
                        estimate.amount_raw.to_string(),
                        estimate.usdc_value.to_string(),
                        estimate.description.as_str(),
                        format_timestamp(estimate.created_at)?,
                    ],
                )
                .map_err(sqlite_error)?;
            Ok(rows == 1)
        })
    }

    /// Aggregate all estimates by category.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when `SQLite` read or decode fails.
    pub fn summary(&self) -> Result<PnlSummary, AppError> {
        self.db.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT category, usdc_value FROM pnl_estimates")
                .map_err(sqlite_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(sqlite_error)?;

            let mut summary = PnlSummary::default();
            for row in rows {
                let (category, value) = row.map_err(sqlite_error)?;
                let category = PnlCategory::try_from(category.as_str())?;
                let value = Decimal::from_str(&value)
                    .map_err(|error| AppError::persistence(error.to_string()))?;
                match category {
                    PnlCategory::RealizedSpread => summary.realized_spread_usdc += value,
                    PnlCategory::Fees => summary.fees_usdc += value,
                    PnlCategory::RebalanceCost => summary.rebalance_cost_usdc += value,
                    PnlCategory::HedgeCost => summary.hedge_cost_usdc += value,
                }
            }
            summary.net_usdc = summary.realized_spread_usdc
                + summary.fees_usdc
                + summary.rebalance_cost_usdc
                + summary.hedge_cost_usdc;
            Ok(summary)
        })
    }
}

/// Runtime event consumer for USDC-estimated P&L.
pub struct PnlEventConsumer<'db> {
    repository: PnlRepository<'db>,
    registry: AssetRegistry,
    price_book: PnlPriceBook,
}

impl<'db> PnlEventConsumer<'db> {
    /// Create a P&L event consumer.
    #[must_use]
    pub fn new(db: &'db Db, registry: AssetRegistry) -> Self {
        Self {
            repository: PnlRepository::new(db),
            registry,
            price_book: PnlPriceBook::default(),
        }
    }

    /// Consume one runtime event.
    ///
    /// # Errors
    ///
    /// Returns an error when a cost-bearing event cannot be valued or persisted.
    pub fn consume(&mut self, event: &RuntimeEvent) -> Result<usize, AppError> {
        match event {
            RuntimeEvent::Swap(SwapEvent::PriceObserved { price, .. }) => {
                self.price_book.record_price(price);
                Ok(0)
            }
            RuntimeEvent::Swap(SwapEvent::Quoted { metadata, quote }) => {
                let Some(fee) = &quote.estimated_fee else {
                    return Ok(0);
                };
                let fee_value = self.price_book.estimate_usdc(&self.registry, fee)?;
                let estimate = PnlEstimate::new(
                    metadata.event_id,
                    "swap_quote_estimated_fee",
                    metadata.event_id,
                    PnlCategory::RebalanceCost,
                    fee,
                    -fee_value.abs(),
                    "Jupiter estimated fee",
                    metadata.occurred_at,
                );
                self.repository.save_estimate(&estimate).map(usize::from)
            }
            _ => Ok(0),
        }
    }
}

fn format_timestamp(timestamp: OffsetDateTime) -> Result<String, AppError> {
    timestamp
        .format(&Rfc3339)
        .map_err(|error| AppError::persistence(error.to_string()))
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use time::macros::datetime;

    use super::*;
    use crate::domain::assets::AssetRegistry;
    use crate::domain::events::{EventMetadata, RuntimeEvent, SwapEvent};
    use crate::domain::types::{AmountRaw, RuntimeRunId, SwapQuote, SwapRequest, WalletRole};

    fn registry() -> AssetRegistry {
        AssetRegistry::default()
    }

    fn usdc() -> AssetId {
        AssetId::from(USDC_ID)
    }

    fn sol() -> AssetId {
        AssetId::from("SOL")
    }

    #[test]
    fn pnl_estimates_usdc_value_from_direct_price() {
        let mut book = PnlPriceBook::default();
        book.record_price(&ReferencePrice {
            pair: AssetPair::new(sol(), usdc()),
            output_per_input: Decimal::new(20, 0),
            observed_at: datetime!(2026-04-30 00:00 UTC),
        });

        let value = book
            .estimate_usdc(
                &registry(),
                &TokenAmount::new(sol(), AmountRaw::new(1_500_000_000)),
            )
            .unwrap();

        assert_eq!(value, Decimal::new(30, 0));
    }

    #[test]
    fn pnl_estimates_usdc_value_from_inverse_price() {
        let mut book = PnlPriceBook::default();
        book.record_price(&ReferencePrice {
            pair: AssetPair::new(usdc(), sol()),
            output_per_input: Decimal::new(5, 2),
            observed_at: datetime!(2026-04-30 00:00 UTC),
        });

        let value = book
            .estimate_usdc(
                &registry(),
                &TokenAmount::new(sol(), AmountRaw::new(1_500_000_000)),
            )
            .unwrap();

        assert_eq!(value, Decimal::new(30, 0));
    }

    #[test]
    fn pnl_summary_calculates_net_usdc() {
        let db = Db::open_in_memory().unwrap();
        let repository = PnlRepository::new(&db);
        let event_id = Uuid::new_v4();
        let now = datetime!(2026-04-30 00:00 UTC);

        repository
            .save_estimate(&PnlEstimate::new(
                event_id,
                "trade",
                Uuid::new_v4(),
                PnlCategory::RealizedSpread,
                &TokenAmount::new(usdc(), AmountRaw::new(2_000_000)),
                Decimal::new(2, 0),
                "spread",
                now,
            ))
            .unwrap();
        repository
            .save_estimate(&PnlEstimate::new(
                Uuid::new_v4(),
                "fee",
                Uuid::new_v4(),
                PnlCategory::Fees,
                &TokenAmount::new(usdc(), AmountRaw::new(100_000)),
                Decimal::new(-1, 1),
                "gas",
                now,
            ))
            .unwrap();
        repository
            .save_estimate(&PnlEstimate::new(
                Uuid::new_v4(),
                "rebalance",
                Uuid::new_v4(),
                PnlCategory::RebalanceCost,
                &TokenAmount::new(usdc(), AmountRaw::new(200_000)),
                Decimal::new(-2, 1),
                "jupiter",
                now,
            ))
            .unwrap();

        let summary = repository.summary().unwrap();

        assert_eq!(summary.realized_spread_usdc, Decimal::new(2, 0));
        assert_eq!(summary.fees_usdc, Decimal::new(-1, 1));
        assert_eq!(summary.rebalance_cost_usdc, Decimal::new(-2, 1));
        assert_eq!(summary.net_usdc, Decimal::new(17, 1));
    }

    #[test]
    fn pnl_swap_quote_fee_is_consumed_as_rebalance_cost() {
        let db = Db::open_in_memory().unwrap();
        let mut consumer = PnlEventConsumer::new(&db, registry());
        let run_id = RuntimeRunId::generate();

        consumer
            .consume(&RuntimeEvent::Swap(SwapEvent::PriceObserved {
                metadata: EventMetadata::new(run_id),
                price: ReferencePrice {
                    pair: AssetPair::new(sol(), usdc()),
                    output_per_input: Decimal::new(20, 0),
                    observed_at: datetime!(2026-04-30 00:00 UTC),
                },
            }))
            .unwrap();

        let quote_event_id = Uuid::new_v4();
        let inserted = consumer
            .consume(&RuntimeEvent::Swap(SwapEvent::Quoted {
                metadata: EventMetadata {
                    event_id: quote_event_id,
                    run_id,
                    occurred_at: datetime!(2026-04-30 00:01 UTC),
                },
                quote: SwapQuote {
                    request: SwapRequest {
                        pair: AssetPair::new(usdc(), sol()),
                        input_amount: TokenAmount::new(usdc(), AmountRaw::new(1_000_000)),
                        source_wallet: WalletRole::Maker,
                        destination_wallet: WalletRole::Maker,
                        max_slippage_bps: 50,
                    },
                    expected_output: TokenAmount::new(sol(), AmountRaw::new(50_000_000)),
                    estimated_fee: Some(TokenAmount::new(sol(), AmountRaw::new(1_000_000))),
                    expires_at: None,
                },
            }))
            .unwrap();

        assert_eq!(inserted, 1);

        let summary = PnlRepository::new(&db).summary().unwrap();
        assert_eq!(summary.rebalance_cost_usdc, Decimal::new(-2, 2));
        assert_eq!(summary.net_usdc, Decimal::new(-2, 2));
    }
}
