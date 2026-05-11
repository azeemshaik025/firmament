//! Wallet-side reconciliation: `working_custody` + `reserved` + `pending_dex_spend`
//! vs. on-chain.

use std::{collections::HashMap, sync::Arc};

use time::OffsetDateTime;

use crate::adapters::persistence::ledger::{LedgerAccountId, LedgerAccountType};
use crate::application::runtime::orchestrator::{RuntimeOrchestrator, RuntimePersistence};
use crate::config::ReconciliationConfig;
use crate::domain::events::{
    EventMetadata, ReconciliationEvent, ReconciliationOutcome, ReconciliationScope, RuntimeEvent,
};
use crate::domain::types::{AmountRaw, AssetId, TokenAmount, WalletRole};
use crate::error::{AppError, AppResult};

use super::drift::{DriftOutcome, DriftWindow};
use super::{SequenceMap, format_utc_date};

/// Outcome surfaced to callers (mirrors [`ReconciliationOutcome`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationOutcome {
    /// Drift was within dust; no adjustment posted.
    WithinDust,
    /// Drift was above dust; window not yet at threshold.
    Building {
        /// Number of consecutive same-sign observations recorded.
        observations: u8,
    },
    /// Adjustment was posted with this idempotency key.
    Adjusted {
        /// Idempotency key under which the adjustment was persisted.
        idempotency_key: String,
    },
    /// Adjustment was triggered but blocked by a hard guard.
    Skipped {
        /// Stable, operator-facing reason.
        reason: String,
    },
}

/// One wallet observation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletObservation {
    /// Asset that was observed.
    pub asset: AssetId,
    /// On-chain raw amount.
    pub on_chain_raw: u128,
    /// Expected raw amount derived from the ledger.
    pub expected_raw: u128,
    /// Signed drift `on_chain - expected`.
    pub drift_raw: i128,
    /// Outcome of the observation.
    pub outcome: ObservationOutcome,
}

/// Wallet-side reconciliation monitor. One per worker; owns the per-asset
/// drift windows and contributes to the shared daily sequence map.
pub struct WalletMonitor {
    windows: HashMap<AssetId, DriftWindow>,
    dust_per_asset: HashMap<AssetId, u128>,
    default_dust: u128,
    threshold: u8,
    emit_event_on_skip: bool,
    sequences: SequenceMap,
}

impl WalletMonitor {
    /// Build a wallet monitor seeded with daily sequence counters from the
    /// shared map.
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

    /// Run one tick for the supplied asset.
    ///
    /// # Errors
    ///
    /// Returns adapter, ledger, or event-publish errors.
    pub async fn tick(
        &mut self,
        orchestrator: &RuntimeOrchestrator,
        asset: &AssetId,
    ) -> AppResult<WalletObservation> {
        let on_chain_raw = read_on_chain_balance(orchestrator, asset).await?;
        let expected_raw = self.expected_balance(orchestrator, asset)?;

        let drift_raw =
            i128::try_from(on_chain_raw).unwrap_or(i128::MAX) - i128_from_u128(expected_raw);

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
                if let Some(reason) = trade_in_flight_reason(orchestrator, asset)? {
                    // Reset the window so the next tick starts a fresh count
                    // once the in-flight trade settles.
                    window.reset();
                    ObservationOutcome::Skipped { reason }
                } else {
                    let key = self.next_idempotency_key(asset);
                    let posted = post_wallet_adjustment(orchestrator, asset, drift, &key)?;
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

        let observation = WalletObservation {
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
    ) -> AppResult<u128> {
        let persistence = required_persistence(orchestrator.persistence_handle())?;

        let working = persistence.account_balance(&LedgerAccountId::working(asset.clone()))?;
        let reserved = persistence.aggregate_balance_by_type(LedgerAccountType::Reserved, asset)?;
        let pending_dex_spend =
            persistence.aggregate_balance_by_type(LedgerAccountType::PendingDexSpend, asset)?;

        let signed = working
            .saturating_add(reserved.max(0))
            .saturating_add(pending_dex_spend.max(0));
        Ok(u128::try_from(signed.max(0)).unwrap_or(0))
    }

    fn next_idempotency_key(&mut self, asset: &AssetId) -> String {
        let date = format_utc_date(OffsetDateTime::now_utc());
        let entry = self
            .sequences
            .entry(("wallet", asset.clone(), date.clone()))
            .or_insert(0);
        *entry = entry.saturating_add(1);
        format!("recon:wallet:{}:{date}:{}", asset.as_str(), *entry)
    }

    async fn publish_tick(
        &self,
        orchestrator: &RuntimeOrchestrator,
        observation: &WalletObservation,
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
                scope: ReconciliationScope::Wallet,
                on_chain_raw: observation.on_chain_raw.to_string(),
                expected_raw: observation.expected_raw.to_string(),
                drift_raw: observation.drift_raw.to_string(),
                outcome,
            }))
            .await
    }
}

fn i128_from_u128(value: u128) -> i128 {
    i128::try_from(value).unwrap_or(i128::MAX)
}

fn required_persistence(
    persistence: Option<Arc<RuntimePersistence>>,
) -> AppResult<Arc<RuntimePersistence>> {
    persistence.ok_or_else(|| AppError::persistence("reconciliation requires persistence"))
}

async fn read_on_chain_balance(
    orchestrator: &RuntimeOrchestrator,
    asset: &AssetId,
) -> AppResult<u128> {
    let snapshot = orchestrator.balances(WalletRole::Maker).await?;
    Ok(snapshot
        .balances
        .iter()
        .find(|amount: &&TokenAmount| amount.asset == *asset)
        .map_or(0, |amount| u128::from(amount.amount_raw.as_u64())))
}

fn trade_in_flight_reason(
    orchestrator: &RuntimeOrchestrator,
    asset: &AssetId,
) -> AppResult<Option<String>> {
    let persistence = required_persistence(orchestrator.persistence_handle())?;

    let pending_escrow =
        persistence.aggregate_balance_by_type(LedgerAccountType::PendingEscrow, asset)?;
    let htlc_escrow =
        persistence.aggregate_balance_by_type(LedgerAccountType::HtlcEscrow, asset)?;
    let receivable = persistence.aggregate_balance_by_type(LedgerAccountType::Receivable, asset)?;
    let pending_dex_spend =
        persistence.aggregate_balance_by_type(LedgerAccountType::PendingDexSpend, asset)?;

    let combined = pending_escrow
        .saturating_add(htlc_escrow)
        .saturating_add(receivable)
        .saturating_add(pending_dex_spend);
    Ok((combined > 0).then(|| "trade_in_flight".to_owned()))
}

fn post_wallet_adjustment(
    orchestrator: &RuntimeOrchestrator,
    asset: &AssetId,
    drift: i128,
    idempotency_key: &str,
) -> AppResult<bool> {
    let persistence = required_persistence(orchestrator.persistence_handle())?;
    let magnitude_u64 = u64::try_from(drift.unsigned_abs().min(u128::from(u64::MAX))).unwrap_or(0);
    if magnitude_u64 == 0 {
        return Ok(false);
    }
    let amount = AmountRaw::new(magnitude_u64);

    let working = LedgerAccountId::working(asset.clone());
    let external = LedgerAccountId::external(asset.clone(), "reconciliation");

    let builder = crate::adapters::persistence::ledger::LedgerTransactionBuilder::new(
        "reconciliation_wallet",
        uuid::Uuid::now_v7(),
    )
    .description(if drift >= 0 {
        "wallet drift adjustment: external:reconciliation -> working_custody"
    } else {
        "wallet drift adjustment: working_custody -> external:reconciliation"
    })
    .idempotency_key(idempotency_key.to_owned());

    let transaction = if drift >= 0 {
        builder
            .debit(working, amount)
            .credit(external, amount)
            .build()?
    } else {
        builder
            .debit(external, amount)
            .credit(working, amount)
            .build()?
    };

    let outcome = persistence.save_ledger_transaction(&transaction)?;
    Ok(matches!(
        outcome,
        crate::adapters::persistence::ledger::LedgerSaveOutcome::Inserted { .. }
    ))
}

#[cfg(test)]
mod tests {
    use crate::error::AppError;

    use super::required_persistence;

    #[test]
    fn missing_persistence_returns_error_instead_of_panicking() {
        let Err(error) = required_persistence(None) else {
            panic!("missing persistence should error");
        };

        assert!(matches!(
            error,
            AppError::Persistence(message) if message == "reconciliation requires persistence"
        ));
    }
}
