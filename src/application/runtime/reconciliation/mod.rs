//! Always-on Solana reconciliation worker.
//!
//! See `docs/plans/2026-05-07-firmament-rfq-runtime-design.md` § 3 for the
//! full design and `docs/plans/2026-05-07-firmament-rfq-runtime-implementation.md`
//! Worktree A for the task breakdown.
//!
//! The worker compares the on-chain wallet and Circle Gateway state against
//! the ledger's expected balances each tick, and posts balanced ledger
//! adjustments under deterministic idempotency keys when drift exceeds a
//! per-asset dust threshold over a configurable consecutive-observation
//! window. Every tick emits a [`crate::domain::events::ReconciliationEvent`]
//! for operator visibility.

pub mod drift;
pub mod gateway_monitor;
pub mod wallet_monitor;

pub use drift::{DriftOutcome, DriftWindow};
pub use gateway_monitor::{GatewayMonitor, GatewayObservation};
pub use wallet_monitor::{ObservationOutcome, WalletMonitor, WalletObservation};

use std::collections::HashMap;
use std::sync::Arc;

use time::OffsetDateTime;

use crate::application::runtime::orchestrator::RuntimeOrchestrator;
use crate::config::ReconciliationConfig;
use crate::domain::types::AssetId;
use crate::error::AppResult;

/// Daily idempotency-sequence counter, keyed by `(scope, asset, utc_date)`.
pub(crate) type SequenceMap = HashMap<(&'static str, AssetId, String), u64>;

/// Always-on reconciliation worker handle.
///
/// Owns the per-asset `WalletMonitor` and the singleton `GatewayMonitor`,
/// plus the daily idempotency-sequence map seeded from the ledger at
/// startup.
pub struct ReconciliationWorker {
    orchestrator: Arc<RuntimeOrchestrator>,
    config: ReconciliationConfig,
    wallet: WalletMonitor,
    gateway: GatewayMonitor,
}

impl ReconciliationWorker {
    /// Build a new worker. Reseeds the daily idempotency-sequence counters
    /// from any persisted reconciliation transactions for today's UTC date.
    ///
    /// # Errors
    ///
    /// Returns persistence errors when the ledger reseed query fails.
    pub fn new(
        orchestrator: Arc<RuntimeOrchestrator>,
        config: ReconciliationConfig,
    ) -> AppResult<Self> {
        let today = today_utc_date();
        let mut sequences: SequenceMap = HashMap::new();
        if let Some(persistence) = orchestrator.persistence_handle() {
            persistence.seed_recon_sequences(&today, &mut sequences)?;
        }

        let wallet = WalletMonitor::new(&config, sequences.clone());
        let gateway = GatewayMonitor::new(&config, sequences);

        Ok(Self {
            orchestrator,
            config,
            wallet,
            gateway,
        })
    }

    /// Run one wallet observation for a specific asset. Used by integration
    /// tests that drive the worker deterministically.
    ///
    /// # Errors
    ///
    /// Returns adapter, ledger, or event-publish errors.
    pub async fn tick_wallet(&mut self, asset: &AssetId) -> AppResult<WalletObservation> {
        self.wallet.tick(self.orchestrator.as_ref(), asset).await
    }

    /// Run one Gateway observation for the supplied asset (USDC). Used by
    /// integration tests that drive the worker deterministically.
    ///
    /// # Errors
    ///
    /// Returns adapter, ledger, or event-publish errors.
    pub async fn tick_gateway(&mut self, asset: &AssetId) -> AppResult<GatewayObservation> {
        self.gateway.tick(self.orchestrator.as_ref(), asset).await
    }

    /// Borrow the active reconciliation config.
    #[must_use]
    pub const fn config(&self) -> &ReconciliationConfig {
        &self.config
    }
}

/// UTC date in `YYYY-MM-DD` form, used in idempotency keys.
#[must_use]
pub fn today_utc_date() -> String {
    format_utc_date(OffsetDateTime::now_utc())
}

#[must_use]
pub(super) fn format_utc_date(now: OffsetDateTime) -> String {
    let date = now.date();
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}
