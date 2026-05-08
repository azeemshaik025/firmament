//! RFQ request handling and quote lifecycle helpers.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::{AppConfig, AssetConfig};
use uuid::Uuid;

use crate::domain::events::{EventMetadata, QuoteEvent, RiskEvent, RuntimeEvent};
use crate::domain::quote_engine::{
    InventorySnapshot, QuoteBreakdown, QuoteEngineConfig, QuoteMathError, calculate_quote,
};
use crate::domain::risk::{RiskEvaluationInput, RiskPolicy, evaluate_pre_quote};
use crate::domain::types::{
    AmountRaw, AssetPair, ExecutionPath, MintAddress, QuoteId, ReferencePrice, RejectionReason,
    RiskDecision, RuntimeRunId, TokenAmount, TradeId, WalletAddress,
};
use crate::ports::PriceProvider;

/// RFQ request body accepted by the domain layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RfqRequest {
    /// Input token mint address.
    pub input_mint: MintAddress,
    /// Output token mint address.
    pub output_mint: MintAddress,
    /// Input amount in raw token units.
    pub input_amount_raw: AmountRaw,
    /// Taker wallet requesting the quote.
    pub taker_wallet: WalletAddress,
    /// Optional quote expiry override in seconds.
    pub expiry_seconds: Option<u64>,
}

/// Context required for deterministic RFQ evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfqContext {
    /// Supported asset metadata.
    pub assets: Vec<AssetConfig>,
    /// Quote math config.
    pub quote_config: QuoteEngineConfig,
    /// Risk policy config.
    pub risk_policy: RiskPolicy,
    /// Current working inventory.
    pub inventory: InventorySnapshot,
    /// Default quote expiry in seconds.
    pub default_expiry_seconds: u64,
}

impl RfqContext {
    /// Build an RFQ context from non-secret app config and a live inventory snapshot.
    #[must_use]
    pub fn from_config(config: &AppConfig, inventory: InventorySnapshot) -> Self {
        let quote_config = QuoteEngineConfig {
            base_spread_bps: i32::from(config.assets.policy.base_spread_bps),
            ..QuoteEngineConfig::default()
        };

        let risk_policy = RiskPolicy {
            supported_pairs: config.assets.enabled_pairs(),
            allowlisted_takers: Vec::new(),
            require_taker_allowlist: config.risk.require_taker_allowlist,
            max_quote_notional_usd: Some(config.risk.max_quote_notional_usd),
            max_price_staleness_seconds: config.risk.max_price_staleness_seconds,
            quoteable_thresholds: config
                .assets
                .supported
                .iter()
                .map(|asset| TokenAmount::new(asset.id.clone(), asset.quoteable_threshold_raw))
                .collect(),
            exposure_limits_usd: config
                .assets
                .supported
                .iter()
                .map(|asset| {
                    let limit = if matches!(asset.id.as_str(), "USDC" | "SOL") {
                        config.risk.max_trade_notional_usd
                    } else {
                        config.risk.max_non_stable_asset_notional_usd
                    };
                    (asset.id.clone(), limit)
                })
                .collect(),
            current_exposure_usd: config
                .assets
                .supported
                .iter()
                .map(|asset| (asset.id.clone(), Decimal::ZERO))
                .collect(),
            native_sol_gas_buffer_raw: config
                .assets
                .supported
                .iter()
                .find(|asset| asset.id.as_str() == "SOL")
                .map(|asset| asset.quoteable_threshold_raw)
                .unwrap_or_default(),
            asset_usd_prices: vec![(crate::domain::types::AssetId::from("USDC"), Decimal::ONE)],
        };

        Self {
            assets: config.assets.supported.clone(),
            quote_config,
            risk_policy,
            inventory,
            default_expiry_seconds: config.assets.policy.default_quote_expiry_seconds,
        }
    }
}

/// Minimal HTLC acceptance data returned with a firm quote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtlcAcceptanceTerms {
    /// Taker wallet expected to fund settlement.
    pub taker_wallet: WalletAddress,
    /// Maker receives this amount from the taker.
    pub maker_receive: TokenAmount,
    /// Maker pays this amount from working inventory.
    pub maker_pay: TokenAmount,
    /// Quote expiry that should bound settlement initiation.
    pub expires_at: OffsetDateTime,
}

/// Firm quote terms produced by the maker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirmQuote {
    /// Quote identifier.
    pub quote_id: QuoteId,
    /// Directional pair.
    pub pair: AssetPair,
    /// Input amount from taker.
    pub input_amount: TokenAmount,
    /// Output amount quoted by maker.
    pub output_amount: TokenAmount,
    /// Applied total spread.
    pub spread_bps: i32,
    /// Expiry timestamp.
    pub expires_at: OffsetDateTime,
    /// Reference price used.
    pub reference_price: ReferencePrice,
    /// Full quote math breakdown.
    pub breakdown: QuoteBreakdown,
    /// Accepted risk decision details.
    pub risk_decision: RiskDecision,
    /// Settlement terms for later HTLC flow.
    pub htlc_terms: HtlcAcceptanceTerms,
    /// How the maker plans to source the output (inventory vs Gateway-backed).
    pub execution_path: ExecutionPath,
}

/// Structured quote rejection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedQuote {
    /// Quote identifier if assigned.
    pub quote_id: Option<QuoteId>,
    /// Stable rejection reason.
    pub reason: RejectionReason,
    /// Full risk decision details.
    pub decision: RiskDecision,
}

/// RFQ response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RfqResponse {
    /// RFQ accepted and firm quote terms returned.
    Accepted(Box<FirmQuote>),
    /// RFQ rejected with structured risk details.
    Rejected(RejectedQuote),
}

/// RFQ response plus runtime events for projections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RfqOutcome {
    /// Domain response.
    pub response: RfqResponse,
    /// Quote/risk events emitted by this evaluation.
    pub events: Vec<RuntimeEvent>,
}

/// Quote acceptance attempt result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QuoteAcceptance {
    /// Quote is still live and was accepted for settlement orchestration.
    Accepted {
        /// Trade identifier assigned to the future settlement.
        trade_id: TradeId,
        /// Event emitted for runtime projections.
        event: RuntimeEvent,
    },
    /// Quote expired before acceptance.
    Expired {
        /// Structured rejection decision.
        decision: RiskDecision,
        /// Event emitted for runtime projections.
        event: RuntimeEvent,
    },
}

/// Request a firm RFQ quote without submitting settlement or swaps.
#[must_use]
#[allow(clippy::too_many_lines)]
pub async fn request_quote<P: PriceProvider + ?Sized>(
    request: RfqRequest,
    context: &RfqContext,
    price_provider: &P,
    run_id: RuntimeRunId,
    now: OffsetDateTime,
) -> RfqOutcome {
    let quote_id = QuoteId::generate();

    let Some(input_asset) = asset_for_mint(context, &request.input_mint) else {
        return rejected_outcome(
            quote_id,
            run_id,
            now,
            RejectionReason::UnsupportedAsset,
            "input mint is not configured or enabled".to_owned(),
            Vec::new(),
        );
    };
    let Some(output_asset) = asset_for_mint(context, &request.output_mint) else {
        return rejected_outcome(
            quote_id,
            run_id,
            now,
            RejectionReason::UnsupportedAsset,
            "output mint is not configured or enabled".to_owned(),
            Vec::new(),
        );
    };

    let pair = AssetPair::new(input_asset.id.clone(), output_asset.id.clone());
    let expiry_seconds = request
        .expiry_seconds
        .unwrap_or(context.default_expiry_seconds);
    let expires_at = add_seconds(now, expiry_seconds).unwrap_or(now);
    let input_amount = TokenAmount::new(pair.input.clone(), request.input_amount_raw);
    let mut events = vec![RuntimeEvent::Quote(QuoteEvent::Requested {
        metadata: metadata(run_id, now),
        quote_id,
        pair: pair.clone(),
        input_amount: input_amount.clone(),
        taker_wallet: request.taker_wallet.clone(),
        expires_at,
    })];

    let reference_price = match price_provider.reference_price(pair.clone()).await {
        Ok(price) => price,
        Err(error) => {
            return rejected_outcome(
                quote_id,
                run_id,
                now,
                RejectionReason::ExternalServiceUnavailable,
                format!("reference price unavailable: {error}"),
                events,
            );
        }
    };

    let quote = match calculate_quote(
        pair.clone(),
        request.input_amount_raw,
        reference_price.clone(),
        input_asset.decimals,
        output_asset.decimals,
        &context.inventory,
        &context.quote_config,
    ) {
        Ok(quote) => quote,
        Err(error) => {
            let (reason, detail) = quote_math_rejection(error);
            return rejected_outcome(quote_id, run_id, now, reason, detail, events);
        }
    };

    let Some(notional_usd) = estimate_notional_usd(
        &quote.input_amount,
        &quote.output_amount,
        input_asset.decimals,
        output_asset.decimals,
        &context.risk_policy,
    ) else {
        return rejected_outcome(
            quote_id,
            run_id,
            now,
            RejectionReason::ValidationFailed,
            format!(
                "missing USD valuation for pair {} -> {}",
                pair.input, pair.output
            ),
            events,
        );
    };

    let risk_input = RiskEvaluationInput {
        pair: pair.clone(),
        taker_wallet: request.taker_wallet.clone(),
        input_amount: quote.input_amount.clone(),
        output_amount: quote.output_amount.clone(),
        reference_price: reference_price.clone(),
        notional_usd,
        now,
        expires_at,
        inventory: context.inventory.clone(),
    };
    let risk_decision = evaluate_pre_quote(&risk_input, &context.risk_policy);
    events.push(RuntimeEvent::Risk(RiskEvent::Evaluated {
        metadata: metadata(run_id, now),
        quote_id: Some(quote_id),
        decision: risk_decision.clone(),
    }));

    if let RiskDecision::Rejected { reason, .. } = &risk_decision {
        events.push(RuntimeEvent::Quote(QuoteEvent::Rejected {
            metadata: metadata(run_id, now),
            quote_id: Some(quote_id),
            reason: reason.clone(),
            decision: risk_decision.clone(),
        }));
        return RfqOutcome {
            response: RfqResponse::Rejected(RejectedQuote {
                quote_id: Some(quote_id),
                reason: reason.clone(),
                decision: risk_decision,
            }),
            events,
        };
    }

    let firm_quote = FirmQuote {
        quote_id,
        pair,
        input_amount: quote.input_amount.clone(),
        output_amount: quote.output_amount.clone(),
        spread_bps: quote.breakdown.total_spread_bps,
        expires_at,
        reference_price,
        breakdown: quote.breakdown,
        risk_decision,
        htlc_terms: HtlcAcceptanceTerms {
            taker_wallet: request.taker_wallet,
            maker_receive: quote.input_amount,
            maker_pay: quote.output_amount,
            expires_at,
        },
        // Default. The orchestrator overrides this with the resolved path
        // (`InventoryToInventory` vs `GatewayToDex`) once it knows whether
        // the requested output is covered by working_custody alone.
        execution_path: ExecutionPath::InventoryToInventory,
    };

    RfqOutcome {
        response: RfqResponse::Accepted(Box::new(firm_quote)),
        events,
    }
}

/// Outcome of `select_execution_path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPathSelection {
    /// A path was resolved.
    Path(ExecutionPath),
    /// Neither inventory nor Gateway-backed supply covers the output amount.
    InsufficientLiquidity,
}

/// Resolve which execution path can fund the maker output for a quote.
///
/// Rules (matches design § 2 / Worktree C T-C1):
/// 1. If `working_custody_output_raw >= requested_output_raw` → [`ExecutionPath::InventoryToInventory`].
/// 2. Else if `gateway_free_output_equivalent_raw >= requested_output_raw` → [`ExecutionPath::GatewayToDex`].
/// 3. Else [`ExecutionPathSelection::InsufficientLiquidity`].
///
/// `gateway_free_output_equivalent_raw` is the FREE Gateway USDC
/// (`gateway − sum(gateway_reserved)`) translated into the output asset's
/// raw units via the reference price. Callers performing the conversion
/// should clamp to zero on overflow.
#[must_use]
pub fn select_execution_path(
    working_custody_output_raw: u64,
    requested_output_raw: u64,
    gateway_free_output_equivalent_raw: u64,
) -> ExecutionPathSelection {
    if working_custody_output_raw >= requested_output_raw {
        ExecutionPathSelection::Path(ExecutionPath::InventoryToInventory)
    } else if gateway_free_output_equivalent_raw >= requested_output_raw {
        ExecutionPathSelection::Path(ExecutionPath::GatewayToDex)
    } else {
        ExecutionPathSelection::InsufficientLiquidity
    }
}

/// Accept a quote for later settlement orchestration without submitting on-chain work.
#[must_use]
pub fn accept_quote_for_settlement(
    quote: &FirmQuote,
    run_id: RuntimeRunId,
    now: OffsetDateTime,
) -> QuoteAcceptance {
    if quote.expires_at <= now {
        let decision = RiskDecision::Rejected {
            reason: RejectionReason::ValidationFailed,
            details: vec![format!(
                "quote {} expired at {}",
                quote.quote_id, quote.expires_at
            )],
        };
        return QuoteAcceptance::Expired {
            decision,
            event: RuntimeEvent::Quote(QuoteEvent::Expired {
                metadata: metadata(run_id, now),
                quote_id: quote.quote_id,
            }),
        };
    }

    let trade_id = TradeId::generate();
    QuoteAcceptance::Accepted {
        trade_id,
        event: RuntimeEvent::Quote(QuoteEvent::Accepted {
            metadata: metadata(run_id, now),
            quote_id: quote.quote_id,
            trade_id,
        }),
    }
}

fn asset_for_mint<'ctx>(
    context: &'ctx RfqContext,
    mint: &MintAddress,
) -> Option<&'ctx AssetConfig> {
    context
        .assets
        .iter()
        .find(|asset| asset.enabled && &asset.mint == mint)
}

fn add_seconds(now: OffsetDateTime, seconds: u64) -> Option<OffsetDateTime> {
    let seconds = i64::try_from(seconds).ok()?;
    Some(now + time::Duration::seconds(seconds))
}

fn metadata(run_id: RuntimeRunId, occurred_at: OffsetDateTime) -> EventMetadata {
    EventMetadata {
        event_id: Uuid::now_v7(),
        run_id,
        occurred_at,
    }
}

fn rejected_outcome(
    quote_id: QuoteId,
    run_id: RuntimeRunId,
    now: OffsetDateTime,
    reason: RejectionReason,
    detail: String,
    mut events: Vec<RuntimeEvent>,
) -> RfqOutcome {
    let decision = RiskDecision::Rejected {
        reason: reason.clone(),
        details: vec![detail],
    };
    events.push(RuntimeEvent::Risk(RiskEvent::Evaluated {
        metadata: metadata(run_id, now),
        quote_id: Some(quote_id),
        decision: decision.clone(),
    }));
    events.push(RuntimeEvent::Quote(QuoteEvent::Rejected {
        metadata: metadata(run_id, now),
        quote_id: Some(quote_id),
        reason: reason.clone(),
        decision: decision.clone(),
    }));

    RfqOutcome {
        response: RfqResponse::Rejected(RejectedQuote {
            quote_id: Some(quote_id),
            reason,
            decision,
        }),
        events,
    }
}

fn quote_math_rejection(error: QuoteMathError) -> (RejectionReason, String) {
    match error {
        QuoteMathError::AmountTooSmall => (RejectionReason::AmountTooSmall, error.to_string()),
        QuoteMathError::InvalidReferencePrice
        | QuoteMathError::AmountOverflow
        | QuoteMathError::SpreadTooWide => (RejectionReason::ValidationFailed, error.to_string()),
    }
}

fn estimate_notional_usd(
    input: &TokenAmount,
    output: &TokenAmount,
    input_decimals: u8,
    output_decimals: u8,
    policy: &RiskPolicy,
) -> Option<Decimal> {
    if input.asset.as_str() == "USDC" {
        return raw_to_decimal(input.amount_raw, input_decimals);
    }

    if output.asset.as_str() == "USDC" {
        return raw_to_decimal(output.amount_raw, output_decimals);
    }

    let input_ui = raw_to_decimal(input.amount_raw, input_decimals)?;
    let usd_price = policy
        .asset_usd_prices
        .iter()
        .find(|(asset, _)| asset == &input.asset)
        .map(|(_, price)| *price)?;
    Some(input_ui * usd_price)
}

fn raw_to_decimal(amount: AmountRaw, decimals: u8) -> Option<Decimal> {
    let mut factor = 1_u64;
    for _ in 0..decimals {
        factor = factor.checked_mul(10)?;
    }
    Some(Decimal::from(amount.as_u64()) / Decimal::from(factor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    use crate::domain::events::QuoteEvent;
    use crate::domain::types::AssetId;
    use crate::error::AppError;

    fn usdc() -> AssetId {
        AssetId::from("USDC")
    }

    fn sol() -> AssetId {
        AssetId::from("SOL")
    }

    fn usdc_mint() -> MintAddress {
        MintAddress::new("USDC_MINT")
    }

    fn sol_mint() -> MintAddress {
        MintAddress::new("SOL_MINT")
    }

    fn taker() -> WalletAddress {
        WalletAddress::new("taker111111111111111111111111111111111111111")
    }

    fn blocked_taker() -> WalletAddress {
        WalletAddress::new("blocked111111111111111111111111111111111111")
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_000)
    }

    fn asset(id: AssetId, mint: MintAddress, decimals: u8, threshold: u64) -> AssetConfig {
        AssetConfig {
            id,
            symbol: String::new(),
            mint,
            decimals,
            enabled: true,
            target_weight: Decimal::ZERO,
            quoteable_threshold_raw: AmountRaw::new(threshold),
        }
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

    fn context(sol_balance: u64) -> RfqContext {
        RfqContext {
            assets: vec![
                asset(usdc(), usdc_mint(), 6, 1_000_000),
                asset(sol(), sol_mint(), 9, 1_000_000),
            ],
            quote_config: QuoteEngineConfig {
                base_spread_bps: 30,
                fee_bps: 5,
                min_profit_bps: 20,
                max_inventory_skew_bps: 50,
            },
            risk_policy: RiskPolicy {
                supported_pairs: vec![AssetPair::new(usdc(), sol())],
                allowlisted_takers: vec![taker()],
                require_taker_allowlist: false,
                max_quote_notional_usd: Some(Decimal::new(2, 0)),
                max_price_staleness_seconds: 20,
                quoteable_thresholds: vec![
                    TokenAmount::new(usdc(), AmountRaw::new(1_000_000)),
                    TokenAmount::new(sol(), AmountRaw::new(1_000_000)),
                ],
                exposure_limits_usd: vec![
                    (usdc(), Decimal::new(10, 0)),
                    (sol(), Decimal::new(10, 0)),
                ],
                current_exposure_usd: vec![(usdc(), Decimal::ZERO), (sol(), Decimal::ZERO)],
                native_sol_gas_buffer_raw: AmountRaw::new(20_000_000),
                asset_usd_prices: vec![(usdc(), Decimal::ONE)],
            },
            inventory: inventory(sol_balance),
            default_expiry_seconds: 45,
        }
    }

    fn rfq(amount: u64) -> RfqRequest {
        RfqRequest {
            input_mint: usdc_mint(),
            output_mint: sol_mint(),
            input_amount_raw: AmountRaw::new(amount),
            taker_wallet: taker(),
            expiry_seconds: None,
        }
    }

    #[derive(Debug, Clone)]
    struct FakePriceProvider {
        price: ReferencePrice,
    }

    #[async_trait]
    impl PriceProvider for FakePriceProvider {
        async fn reference_price(&self, pair: AssetPair) -> Result<ReferencePrice, AppError> {
            if pair == self.price.pair {
                Ok(self.price.clone())
            } else {
                Err(AppError::unsupported("fake price unavailable for pair"))
            }
        }
    }

    fn price(observed_at: OffsetDateTime) -> FakePriceProvider {
        FakePriceProvider {
            price: ReferencePrice {
                pair: AssetPair::new(usdc(), sol()),
                output_per_input: Decimal::new(1, 2),
                observed_at,
            },
        }
    }

    fn rejection_reason(outcome: &RfqOutcome) -> Option<&RejectionReason> {
        match &outcome.response {
            RfqResponse::Rejected(rejection) => Some(&rejection.reason),
            RfqResponse::Accepted(_) => None,
        }
    }

    #[tokio::test]
    async fn rfq_accepts_valid_quote() {
        let outcome = request_quote(
            rfq(1_000_000),
            &context(1_000_000_000),
            &price(now()),
            RuntimeRunId::generate(),
            now(),
        )
        .await;

        match outcome.response {
            RfqResponse::Accepted(quote) => {
                assert_eq!(quote.input_amount.amount_raw, AmountRaw::new(1_000_000));
                assert_eq!(quote.output_amount.amount_raw, AmountRaw::new(9_965_000));
                assert_eq!(quote.spread_bps, 35);
                assert!(quote.expires_at > now());
            }
            RfqResponse::Rejected(rejection) => {
                panic!("expected accepted quote, got {rejection:?}");
            }
        }
    }

    #[tokio::test]
    async fn rfq_rejects_oversized_request() {
        let outcome = request_quote(
            rfq(3_000_000),
            &context(1_000_000_000),
            &price(now()),
            RuntimeRunId::generate(),
            now(),
        )
        .await;

        assert_eq!(
            rejection_reason(&outcome),
            Some(&RejectionReason::MaxNotionalExceeded)
        );
    }

    #[tokio::test]
    async fn rfq_rejects_unsupported_pair() {
        let mut context = context(1_000_000_000);
        context.risk_policy.supported_pairs.clear();

        let outcome = request_quote(
            rfq(1_000_000),
            &context,
            &price(now()),
            RuntimeRunId::generate(),
            now(),
        )
        .await;

        assert_eq!(
            rejection_reason(&outcome),
            Some(&RejectionReason::UnsupportedPair)
        );
    }

    #[tokio::test]
    async fn rfq_rejects_stale_price() {
        let outcome = request_quote(
            rfq(1_000_000),
            &context(1_000_000_000),
            &price(now() - time::Duration::seconds(21)),
            RuntimeRunId::generate(),
            now(),
        )
        .await;

        assert_eq!(
            rejection_reason(&outcome),
            Some(&RejectionReason::StalePrice)
        );
    }

    #[tokio::test]
    async fn rfq_rejects_missing_risk_limit() {
        let mut context = context(1_000_000_000);
        context
            .risk_policy
            .exposure_limits_usd
            .retain(|(asset, _)| asset != &sol());

        let outcome = request_quote(
            rfq(1_000_000),
            &context,
            &price(now()),
            RuntimeRunId::generate(),
            now(),
        )
        .await;

        assert_eq!(
            rejection_reason(&outcome),
            Some(&RejectionReason::ValidationFailed)
        );
    }

    #[tokio::test]
    async fn rfq_rejects_insufficient_inventory() {
        let outcome = request_quote(
            rfq(1_000_000),
            &context(5_000_000),
            &price(now()),
            RuntimeRunId::generate(),
            now(),
        )
        .await;

        assert_eq!(
            rejection_reason(&outcome),
            Some(&RejectionReason::InventoryBelowQuoteableThreshold)
        );
    }

    #[tokio::test]
    async fn rfq_rejects_when_sol_gas_buffer_would_be_spent() {
        let outcome = request_quote(
            rfq(1_000_000),
            &context(25_000_000),
            &price(now()),
            RuntimeRunId::generate(),
            now(),
        )
        .await;

        assert_eq!(
            rejection_reason(&outcome),
            Some(&RejectionReason::InventoryBelowQuoteableThreshold)
        );
    }

    #[tokio::test]
    async fn rfq_rejects_taker_not_on_allowlist() {
        let mut request = rfq(1_000_000);
        request.taker_wallet = blocked_taker();
        let mut context = context(1_000_000_000);
        context.risk_policy.require_taker_allowlist = true;

        let outcome = request_quote(
            request,
            &context,
            &price(now()),
            RuntimeRunId::generate(),
            now(),
        )
        .await;

        assert_eq!(
            rejection_reason(&outcome),
            Some(&RejectionReason::WalletNotAllowed)
        );
    }

    #[test]
    fn select_execution_path_prefers_inventory_when_custody_covers() {
        assert_eq!(
            select_execution_path(1_000_000_000, 500_000_000, 0),
            ExecutionPathSelection::Path(ExecutionPath::InventoryToInventory)
        );
    }

    #[test]
    fn select_execution_path_falls_back_to_gateway_when_custody_short() {
        assert_eq!(
            select_execution_path(0, 500_000_000, 600_000_000),
            ExecutionPathSelection::Path(ExecutionPath::GatewayToDex)
        );
    }

    #[test]
    fn select_execution_path_rejects_when_neither_covers() {
        assert_eq!(
            select_execution_path(0, 500_000_000, 100_000_000),
            ExecutionPathSelection::InsufficientLiquidity
        );
    }

    #[test]
    fn rfq_expired_quote_cannot_be_accepted_for_settlement() {
        let reference_price = ReferencePrice {
            pair: AssetPair::new(usdc(), sol()),
            output_per_input: Decimal::new(1, 2),
            observed_at: now(),
        };
        let quote = FirmQuote {
            quote_id: QuoteId::generate(),
            pair: AssetPair::new(usdc(), sol()),
            input_amount: TokenAmount::new(usdc(), AmountRaw::new(1_000_000)),
            output_amount: TokenAmount::new(sol(), AmountRaw::new(9_965_000)),
            spread_bps: 35,
            expires_at: now() - time::Duration::seconds(1),
            reference_price: reference_price.clone(),
            breakdown: QuoteBreakdown {
                reference_output_amount_raw: AmountRaw::new(10_000_000),
                base_spread_bps: 30,
                inventory_skew_bps: 0,
                fee_bps: 5,
                min_profit_bps: 20,
                total_spread_bps: 35,
                reference_price,
            },
            risk_decision: RiskDecision::Accepted { checks: vec![] },
            htlc_terms: HtlcAcceptanceTerms {
                taker_wallet: taker(),
                maker_receive: TokenAmount::new(usdc(), AmountRaw::new(1_000_000)),
                maker_pay: TokenAmount::new(sol(), AmountRaw::new(9_965_000)),
                expires_at: now() - time::Duration::seconds(1),
            },
            execution_path: ExecutionPath::InventoryToInventory,
        };

        let acceptance = accept_quote_for_settlement(&quote, RuntimeRunId::generate(), now());

        assert!(matches!(
            acceptance,
            QuoteAcceptance::Expired {
                event: RuntimeEvent::Quote(QuoteEvent::Expired { .. }),
                ..
            }
        ));
    }
}
