//! Gateway-side reconciliation: `gateway` + `sum(gateway_reserved:*)` vs.
//! Circle Gateway-reported balance.

use std::collections::HashMap;

use time::OffsetDateTime;

use crate::adapters::persistence::ledger::{LedgerAccountId, LedgerAccountType};
use crate::application::runtime::orchestrator::RuntimeOrchestrator;
use crate::config::ReconciliationConfig;
use crate::domain::events::{
    EventMetadata, ReconciliationEvent, ReconciliationOutcome, ReconciliationScope, RuntimeEvent,
};
use crate::domain::types::{AmountRaw, AssetId};
use crate::error::AppResult;

use super::drift::{DriftOutcome, DriftWindow};
use super::wallet_monitor::ObservationOutcome;
use super::{SequenceMap, format_utc_date};

/// One Gateway observation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayObservation {
    /// Asset (always USDC for the current Gateway scope).
    pub asset: AssetId,
    /// Gateway-reported raw amount.
    pub on_chain_raw: u128,
    /// Expected raw amount derived from the ledger.
    pub expected_raw: i128,
    /// Signed drift `on_chain - expected`.
    pub drift_raw: i128,
    /// Outcome of the observation.
    pub outcome: ObservationOutcome,
}

/// Gateway reconciliation monitor (USDC-only for the current scope).
pub struct GatewayMonitor {
    windows: HashMap<AssetId, DriftWindow>,
    dust_per_asset: HashMap<AssetId, u128>,
    default_dust: u128,
    threshold: u8,
    emit_event_on_skip: bool,
    sequences: SequenceMap,
}

impl GatewayMonitor {
    /// Build a Gateway monitor seeded with daily sequence counters.
    #[must_use]
    pub fn new(config: &ReconciliationConfig, sequences: SequenceMap) -> Self {
        let dust_per_asset = config
            .dust
            .iter()
            .map(|(asset, value)| (AssetId::from(asset.as_str()), u128::from(*value)))
            .collect();
        Self {
            windows: HashMap::new(),
            dust_per_asset,
            default_dust: 0,
            threshold: config.consecutive_ticks_for_adjustment.max(1),
            emit_event_on_skip: config.emit_event_on_skip,
            sequences,
        }
    }

    /// Run one tick for the supplied asset (USDC).
    ///
    /// # Errors
    ///
    /// Returns adapter, ledger, or event-publish errors.
    pub async fn tick(
        &mut self,
        orchestrator: &RuntimeOrchestrator,
        asset: &AssetId,
    ) -> AppResult<GatewayObservation> {
        let on_chain_raw = read_gateway_balance(orchestrator, asset).await?;
        let expected_raw = self.expected_balance(orchestrator, asset)?;

        let drift_raw = i128::try_from(on_chain_raw)
            .unwrap_or(i128::MAX)
            .saturating_sub(expected_raw);

        let dust = self.dust_for(asset);
        let window = self
            .windows
            .entry(asset.clone())
            .or_insert_with(|| DriftWindow::with_threshold(dust, self.threshold));

        let outcome = match window.observe(drift_raw) {
            DriftOutcome::WithinDust => ObservationOutcome::WithinDust,
            DriftOutcome::Building { observations } => {
                ObservationOutcome::Building { observations }
            }
            DriftOutcome::Trigger { drift } => {
                let reserved_active = gateway_reserved_active(orchestrator, asset).unwrap_or(false);
                if drift < 0 && reserved_active {
                    window.reset();
                    ObservationOutcome::Skipped {
                        reason: "gateway_reserved_active".to_owned(),
                    }
                } else {
                    let key = self.next_idempotency_key(asset);
                    let posted = post_gateway_adjustment(orchestrator, asset, drift, &key)?;
                    if posted {
                        ObservationOutcome::Adjusted {
                            idempotency_key: key,
                        }
                    } else {
                        ObservationOutcome::Skipped {
                            reason: "idempotent_replay".to_owned(),
                        }
                    }
                }
            }
        };

        let observation = GatewayObservation {
            asset: asset.clone(),
            on_chain_raw,
            expected_raw,
            drift_raw,
            outcome: outcome.clone(),
        };

        self.publish_tick(orchestrator, &observation).await?;
        Ok(observation)
    }

    fn dust_for(&self, asset: &AssetId) -> u128 {
        self.dust_per_asset
            .get(asset)
            .copied()
            .unwrap_or(self.default_dust)
    }

    #[allow(clippy::unused_self)]
    fn expected_balance(
        &self,
        orchestrator: &RuntimeOrchestrator,
        asset: &AssetId,
    ) -> AppResult<i128> {
        let persistence = orchestrator
            .persistence_handle()
            .expect("reconciliation requires persistence");

        let gateway = persistence.account_balance(&LedgerAccountId::gateway(asset.clone()))?;
        let reserved =
            persistence.aggregate_balance_by_type(LedgerAccountType::GatewayReserved, asset)?;

        Ok(gateway.saturating_add(reserved.max(0)))
    }

    fn next_idempotency_key(&mut self, asset: &AssetId) -> String {
        let date = format_utc_date(OffsetDateTime::now_utc());
        let entry = self
            .sequences
            .entry(("gateway", asset.clone(), date.clone()))
            .or_insert(0);
        *entry = entry.saturating_add(1);
        format!("recon:gateway:{}:{date}:{}", asset.as_str(), *entry)
    }

    async fn publish_tick(
        &self,
        orchestrator: &RuntimeOrchestrator,
        observation: &GatewayObservation,
    ) -> AppResult<()> {
        let outcome = match &observation.outcome {
            ObservationOutcome::WithinDust => ReconciliationOutcome::WithinDust,
            ObservationOutcome::Building { observations } => ReconciliationOutcome::Building {
                observations: *observations,
            },
            ObservationOutcome::Adjusted { idempotency_key } => ReconciliationOutcome::Adjusted {
                idempotency_key: idempotency_key.clone(),
            },
            ObservationOutcome::Skipped { reason } => {
                if !self.emit_event_on_skip {
                    return Ok(());
                }
                ReconciliationOutcome::Skipped {
                    reason: reason.clone(),
                }
            }
        };

        orchestrator
            .runtime()
            .publish_event(RuntimeEvent::Reconciliation(ReconciliationEvent::Tick {
                metadata: EventMetadata::new(orchestrator.run_id()),
                asset: observation.asset.clone(),
                scope: ReconciliationScope::Gateway,
                on_chain_raw: observation.on_chain_raw.to_string(),
                expected_raw: observation.expected_raw.to_string(),
                drift_raw: observation.drift_raw.to_string(),
                outcome,
            }))
            .await
    }
}

async fn read_gateway_balance(
    orchestrator: &RuntimeOrchestrator,
    asset: &AssetId,
) -> AppResult<u128> {
    let receipt = orchestrator.gateway_balance(asset.clone()).await?;
    Ok(u128::from(receipt.amount.amount_raw.as_u64()))
}

fn gateway_reserved_active(orchestrator: &RuntimeOrchestrator, asset: &AssetId) -> AppResult<bool> {
    let persistence = orchestrator
        .persistence_handle()
        .expect("reconciliation requires persistence");
    let reserved =
        persistence.aggregate_balance_by_type(LedgerAccountType::GatewayReserved, asset)?;
    Ok(reserved > 0)
}

fn post_gateway_adjustment(
    orchestrator: &RuntimeOrchestrator,
    asset: &AssetId,
    drift: i128,
    idempotency_key: &str,
) -> AppResult<bool> {
    let persistence = orchestrator
        .persistence_handle()
        .expect("reconciliation requires persistence");
    let magnitude_u64 = u64::try_from(drift.unsigned_abs().min(u128::from(u64::MAX))).unwrap_or(0);
    if magnitude_u64 == 0 {
        return Ok(false);
    }
    let amount = AmountRaw::new(magnitude_u64);

    let gateway = LedgerAccountId::gateway(asset.clone());
    let external = LedgerAccountId::external(asset.clone(), "reconciliation");

    let builder = crate::adapters::persistence::ledger::LedgerTransactionBuilder::new(
        "reconciliation_gateway",
        uuid::Uuid::now_v7(),
    )
    .description(if drift >= 0 {
        "gateway drift adjustment: external:reconciliation -> gateway"
    } else {
        "gateway drift adjustment: gateway -> external:reconciliation"
    })
    .idempotency_key(idempotency_key.to_owned());

    let transaction = if drift >= 0 {
        builder
            .debit(gateway, amount)
            .credit(external, amount)
            .build()?
    } else {
        builder
            .debit(external, amount)
            .credit(gateway, amount)
            .build()?
    };

    let outcome = persistence.save_ledger_transaction(&transaction)?;
    Ok(matches!(
        outcome,
        crate::adapters::persistence::ledger::LedgerSaveOutcome::Inserted { .. }
    ))
}
