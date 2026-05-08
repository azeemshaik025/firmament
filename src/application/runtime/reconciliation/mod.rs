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

pub use drift::{DriftOutcome, DriftWindow};
