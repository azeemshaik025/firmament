//! Two-sided HTLC settlement state for the maker runtime.
//!
//! The state model is intentionally pure: it creates the two `HtlcInitiation`
//! requests expected by the `HtlcClient` contract and returns
//! `SettlementEvent`-compatible transitions as receipts arrive. It does not sign
//! transactions, submit RPC calls, or write ledger records.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::domain::events::{EventMetadata, SettlementEvent};
use crate::domain::types::{
    HtlcInitiation, HtlcReceipt, QuoteId, RuntimeRunId, SettlementStatus, TokenAmount, TradeId,
    TxSignature, WalletRole,
};
use crate::error::{AppError, AppResult};

/// Immutable terms needed to run a two-leg HTLC settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementTerms {
    /// Quote accepted by the taker.
    pub quote_id: QuoteId,
    /// Runtime trade identifier.
    pub trade_id: TradeId,
    /// Asset and amount locked first by the taker.
    pub taker_input: TokenAmount,
    /// Asset and amount locked by the maker after validating taker input.
    pub maker_output: TokenAmount,
    /// Taker wallet role.
    pub taker_wallet: WalletRole,
    /// Maker wallet role.
    pub maker_wallet: WalletRole,
    /// Hex-encoded 32-byte hashlock.
    pub secret_hash: String,
    /// Shared refund deadline used by the HTLC initiation contract.
    pub expires_at: OffsetDateTime,
}

impl SettlementTerms {
    /// Build the taker source-asset HTLC initiation request.
    #[must_use]
    pub fn taker_lock_request(&self) -> HtlcInitiation {
        HtlcInitiation {
            trade_id: self.trade_id,
            funder: self.taker_wallet,
            redeemer: self.maker_wallet,
            amount: self.taker_input.clone(),
            hashlock: self.secret_hash.clone(),
            expires_at: self.expires_at,
        }
    }

    /// Build the maker destination-asset HTLC initiation request.
    #[must_use]
    pub fn maker_lock_request(&self) -> HtlcInitiation {
        HtlcInitiation {
            trade_id: self.trade_id,
            funder: self.maker_wallet,
            redeemer: self.taker_wallet,
            amount: self.maker_output.clone(),
            hashlock: self.secret_hash.clone(),
            expires_at: self.expires_at,
        }
    }
}

/// Which HTLC leg a state transition belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementLeg {
    /// Taker input leg, redeemed by the maker.
    TakerInput,
    /// Maker output leg, redeemed by the taker.
    MakerOutput,
}

/// Fine-grained state machine phase for the two-sided flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementPhase {
    /// Terms exist but no event has been emitted.
    Pending,
    /// Settlement has started.
    Started,
    /// Taker input HTLC was initiated.
    TakerLocked,
    /// Maker output HTLC was initiated.
    MakerLocked,
    /// Taker redeemed maker output with the secret.
    TakerRedeemed,
    /// Maker redeemed taker input with the revealed secret.
    Complete,
    /// One or both legs were refunded.
    Refunded,
    /// Settlement failed before completion.
    Failed,
}

/// Step represented by a returned settlement transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementStep {
    /// Flow start.
    Started,
    /// Taker locks source funds (transaction submitted).
    TakerLock,
    /// Taker lock confirmed on chain.
    TakerLockConfirmed,
    /// Maker locks destination funds (transaction submitted).
    MakerLock,
    /// Maker lock confirmed on chain.
    MakerLockConfirmed,
    /// Taker redeems destination funds.
    TakerRedeem,
    /// Maker redeems source funds.
    MakerRedeem,
    /// Refund for a specific leg.
    Refund(SettlementLeg),
    /// Failure transition.
    Failed,
}

/// Persist-ready receipt state for one HTLC leg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementLegState {
    /// Leg represented by this state.
    pub leg: SettlementLeg,
    /// Initiation receipt, if known.
    pub lock_receipt: Option<HtlcReceipt>,
    /// Redemption receipt, if known.
    pub redeem_receipt: Option<HtlcReceipt>,
    /// Refund receipt, if known.
    pub refund_receipt: Option<HtlcReceipt>,
}

impl SettlementLegState {
    fn new(leg: SettlementLeg) -> Self {
        Self {
            leg,
            lock_receipt: None,
            redeem_receipt: None,
            refund_receipt: None,
        }
    }
}

/// Event-compatible transition returned by state-machine methods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementTransition {
    /// Fine-grained step.
    pub step: SettlementStep,
    /// Runtime event payload for API, web app, and event sinks.
    pub event: SettlementEvent,
}

/// Pure state model for the two-sided HTLC flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TwoSidedSettlement {
    /// Immutable settlement terms.
    pub terms: SettlementTerms,
    /// Current state-machine phase.
    pub phase: SettlementPhase,
    /// Taker input HTLC leg.
    pub taker_input_leg: SettlementLegState,
    /// Maker output HTLC leg.
    pub maker_output_leg: SettlementLegState,
    /// Latest failure reason, if any.
    pub failure_reason: Option<String>,
}

impl TwoSidedSettlement {
    /// Create a pending settlement state from immutable terms.
    #[must_use]
    pub fn new(terms: SettlementTerms) -> Self {
        Self {
            terms,
            phase: SettlementPhase::Pending,
            taker_input_leg: SettlementLegState::new(SettlementLeg::TakerInput),
            maker_output_leg: SettlementLegState::new(SettlementLeg::MakerOutput),
            failure_reason: None,
        }
    }

    /// Mark the flow started.
    #[must_use]
    pub fn start(&mut self, run_id: RuntimeRunId) -> SettlementTransition {
        self.phase = SettlementPhase::Started;
        SettlementTransition {
            step: SettlementStep::Started,
            event: SettlementEvent::Started {
                metadata: EventMetadata::new(run_id),
                trade_id: self.terms.trade_id,
                quote_id: self.terms.quote_id,
            },
        }
    }

    /// Record taker source HTLC submission (post-RPC, pre-confirmation).
    ///
    /// Emits [`SettlementEvent::Submitted`]. Pair with
    /// [`Self::confirm_taker_lock`] once the on-chain account is observed.
    #[must_use]
    pub fn record_taker_lock(
        &mut self,
        run_id: RuntimeRunId,
        signature: Option<String>,
    ) -> SettlementTransition {
        self.phase = SettlementPhase::TakerLocked;
        let receipt = self.receipt(
            SettlementLeg::TakerInput,
            self.terms.taker_input.clone(),
            SettlementStatus::Initiated,
            signature,
        );
        self.taker_input_leg.lock_receipt = Some(receipt.clone());
        SettlementTransition {
            step: SettlementStep::TakerLock,
            event: SettlementEvent::Submitted {
                metadata: EventMetadata::new(run_id),
                receipt,
            },
        }
    }

    /// Record taker source HTLC confirmation.
    ///
    /// Reuses the cached lock receipt (mutated by [`Self::record_taker_lock`])
    /// and emits [`SettlementEvent::Confirmed`].
    ///
    /// # Errors
    ///
    /// Returns an internal error when called before
    /// [`Self::record_taker_lock`] has populated the lock receipt.
    pub fn confirm_taker_lock(&self, run_id: RuntimeRunId) -> AppResult<SettlementTransition> {
        let receipt = self.taker_input_leg.lock_receipt.clone().ok_or_else(|| {
            AppError::internal("confirm_taker_lock called before record_taker_lock")
        })?;
        Ok(SettlementTransition {
            step: SettlementStep::TakerLockConfirmed,
            event: SettlementEvent::Confirmed {
                metadata: EventMetadata::new(run_id),
                receipt,
            },
        })
    }

    /// Record maker destination HTLC submission (post-RPC, pre-confirmation).
    ///
    /// Emits [`SettlementEvent::Submitted`]. Pair with
    /// [`Self::confirm_maker_lock`] once the on-chain account is observed.
    #[must_use]
    pub fn record_maker_lock(
        &mut self,
        run_id: RuntimeRunId,
        signature: Option<String>,
    ) -> SettlementTransition {
        self.phase = SettlementPhase::MakerLocked;
        let receipt = self.receipt(
            SettlementLeg::MakerOutput,
            self.terms.maker_output.clone(),
            SettlementStatus::Initiated,
            signature,
        );
        self.maker_output_leg.lock_receipt = Some(receipt.clone());
        SettlementTransition {
            step: SettlementStep::MakerLock,
            event: SettlementEvent::Submitted {
                metadata: EventMetadata::new(run_id),
                receipt,
            },
        }
    }

    /// Record maker destination HTLC confirmation.
    ///
    /// Reuses the cached lock receipt (mutated by [`Self::record_maker_lock`])
    /// and emits [`SettlementEvent::Confirmed`].
    ///
    /// # Errors
    ///
    /// Returns an internal error when called before
    /// [`Self::record_maker_lock`] has populated the lock receipt.
    pub fn confirm_maker_lock(&self, run_id: RuntimeRunId) -> AppResult<SettlementTransition> {
        let receipt = self.maker_output_leg.lock_receipt.clone().ok_or_else(|| {
            AppError::internal("confirm_maker_lock called before record_maker_lock")
        })?;
        Ok(SettlementTransition {
            step: SettlementStep::MakerLockConfirmed,
            event: SettlementEvent::Confirmed {
                metadata: EventMetadata::new(run_id),
                receipt,
            },
        })
    }

    /// Record taker redemption of maker output.
    ///
    /// The receipt's `leg` is `MakerOutput` — the leg identifies WHICH HTLC
    /// was redeemed (the maker's output leg), not who acted. The taker is the
    /// redeemer of that leg.
    #[must_use]
    pub fn record_taker_redeem(
        &mut self,
        run_id: RuntimeRunId,
        signature: Option<String>,
    ) -> SettlementTransition {
        self.phase = SettlementPhase::TakerRedeemed;
        let receipt = self.receipt(
            SettlementLeg::MakerOutput,
            self.terms.maker_output.clone(),
            SettlementStatus::Redeemed,
            signature,
        );
        self.maker_output_leg.redeem_receipt = Some(receipt.clone());
        SettlementTransition {
            step: SettlementStep::TakerRedeem,
            event: SettlementEvent::Redeemed {
                metadata: EventMetadata::new(run_id),
                receipt,
            },
        }
    }

    /// Record maker redemption of taker input.
    ///
    /// The receipt's `leg` is `TakerInput` — the leg identifies WHICH HTLC
    /// was redeemed (the taker's input leg), not who acted. The maker is the
    /// redeemer of that leg.
    #[must_use]
    pub fn record_maker_redeem(
        &mut self,
        run_id: RuntimeRunId,
        signature: Option<String>,
    ) -> SettlementTransition {
        self.phase = SettlementPhase::Complete;
        let receipt = self.receipt(
            SettlementLeg::TakerInput,
            self.terms.taker_input.clone(),
            SettlementStatus::Redeemed,
            signature,
        );
        self.taker_input_leg.redeem_receipt = Some(receipt.clone());
        SettlementTransition {
            step: SettlementStep::MakerRedeem,
            event: SettlementEvent::Redeemed {
                metadata: EventMetadata::new(run_id),
                receipt,
            },
        }
    }

    /// Record a refund for either HTLC leg.
    #[must_use]
    pub fn record_refund(
        &mut self,
        run_id: RuntimeRunId,
        leg: SettlementLeg,
        signature: Option<String>,
    ) -> SettlementTransition {
        self.phase = SettlementPhase::Refunded;
        let amount = match leg {
            SettlementLeg::TakerInput => self.terms.taker_input.clone(),
            SettlementLeg::MakerOutput => self.terms.maker_output.clone(),
        };
        let receipt = self.receipt(leg, amount, SettlementStatus::Refunded, signature);
        match leg {
            SettlementLeg::TakerInput => {
                self.taker_input_leg.refund_receipt = Some(receipt.clone());
            }
            SettlementLeg::MakerOutput => {
                self.maker_output_leg.refund_receipt = Some(receipt.clone());
            }
        }
        SettlementTransition {
            step: SettlementStep::Refund(leg),
            event: SettlementEvent::Refunded {
                metadata: EventMetadata::new(run_id),
                receipt,
            },
        }
    }

    /// Mark the settlement failed.
    #[must_use]
    pub fn fail(
        &mut self,
        run_id: RuntimeRunId,
        reason: impl Into<String>,
    ) -> SettlementTransition {
        self.phase = SettlementPhase::Failed;
        let reason = reason.into();
        self.failure_reason = Some(reason.clone());
        SettlementTransition {
            step: SettlementStep::Failed,
            event: SettlementEvent::Failed {
                metadata: EventMetadata::new(run_id),
                trade_id: self.terms.trade_id,
                reason,
            },
        }
    }

    fn receipt(
        &self,
        leg: SettlementLeg,
        amount: TokenAmount,
        status: SettlementStatus,
        signature: Option<String>,
    ) -> HtlcReceipt {
        HtlcReceipt {
            trade_id: self.terms.trade_id,
            leg,
            amount,
            status,
            signature: signature.map(TxSignature::new),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::{AmountRaw, AssetId, QuoteId, TokenAmount, TradeId, WalletRole};
    use time::OffsetDateTime;

    fn terms() -> SettlementTerms {
        SettlementTerms {
            quote_id: QuoteId::generate(),
            trade_id: TradeId::generate(),
            taker_input: TokenAmount::new(AssetId::from("USDC"), AmountRaw::new(1_000_000)),
            maker_output: TokenAmount::new(AssetId::from("SOL"), AmountRaw::new(500_000)),
            taker_wallet: WalletRole::Taker,
            maker_wallet: WalletRole::Maker,
            secret_hash: "ab".repeat(32),
            expires_at: OffsetDateTime::now_utc(),
        }
    }

    #[test]
    fn settlement_terms_create_two_htlc_initiation_requests() {
        let terms = terms();

        let taker_lock = terms.taker_lock_request();
        let maker_lock = terms.maker_lock_request();

        assert_eq!(taker_lock.trade_id, terms.trade_id);
        assert_eq!(taker_lock.funder, WalletRole::Taker);
        assert_eq!(taker_lock.redeemer, WalletRole::Maker);
        assert_eq!(taker_lock.amount, terms.taker_input);
        assert_eq!(taker_lock.hashlock, terms.secret_hash);

        assert_eq!(maker_lock.trade_id, terms.trade_id);
        assert_eq!(maker_lock.funder, WalletRole::Maker);
        assert_eq!(maker_lock.redeemer, WalletRole::Taker);
        assert_eq!(maker_lock.amount, terms.maker_output);
        assert_eq!(maker_lock.hashlock, terms.secret_hash);
    }

    #[test]
    fn two_sided_state_emits_settlement_events_for_happy_path() {
        let run_id = crate::domain::types::RuntimeRunId::generate();
        let terms = terms();
        let mut settlement = TwoSidedSettlement::new(terms.clone());

        let started = settlement.start(run_id);
        assert_eq!(settlement.phase, SettlementPhase::Started);
        assert!(matches!(
            started.event,
            crate::domain::events::SettlementEvent::Started { trade_id, quote_id, .. }
                if trade_id == terms.trade_id && quote_id == terms.quote_id
        ));

        let taker_lock = settlement.record_taker_lock(run_id, Some("taker-lock-sig".into()));
        assert_eq!(settlement.phase, SettlementPhase::TakerLocked);
        assert!(matches!(taker_lock.step, SettlementStep::TakerLock));
        assert!(matches!(
            taker_lock.event,
            crate::domain::events::SettlementEvent::Submitted { .. }
        ));

        let taker_lock_confirmed = settlement
            .confirm_taker_lock(run_id)
            .expect("confirm_taker_lock returns transition after record_taker_lock");
        assert!(matches!(
            taker_lock_confirmed.step,
            SettlementStep::TakerLockConfirmed
        ));
        assert!(matches!(
            taker_lock_confirmed.event,
            crate::domain::events::SettlementEvent::Confirmed { .. }
        ));

        let maker_lock = settlement.record_maker_lock(run_id, Some("maker-lock-sig".into()));
        assert_eq!(settlement.phase, SettlementPhase::MakerLocked);
        assert!(matches!(maker_lock.step, SettlementStep::MakerLock));
        assert!(matches!(
            maker_lock.event,
            crate::domain::events::SettlementEvent::Submitted { .. }
        ));

        let maker_lock_confirmed = settlement
            .confirm_maker_lock(run_id)
            .expect("confirm_maker_lock returns transition after record_maker_lock");
        assert!(matches!(
            maker_lock_confirmed.step,
            SettlementStep::MakerLockConfirmed
        ));
        assert!(matches!(
            maker_lock_confirmed.event,
            crate::domain::events::SettlementEvent::Confirmed { .. }
        ));

        let taker_redeem = settlement.record_taker_redeem(run_id, Some("taker-redeem-sig".into()));
        assert_eq!(settlement.phase, SettlementPhase::TakerRedeemed);
        assert!(matches!(taker_redeem.step, SettlementStep::TakerRedeem));
        assert!(matches!(
            taker_redeem.event,
            crate::domain::events::SettlementEvent::Redeemed { .. }
        ));

        let maker_redeem = settlement.record_maker_redeem(run_id, Some("maker-redeem-sig".into()));
        assert_eq!(settlement.phase, SettlementPhase::Complete);
        assert!(matches!(maker_redeem.step, SettlementStep::MakerRedeem));
        assert!(matches!(
            maker_redeem.event,
            crate::domain::events::SettlementEvent::Redeemed { .. }
        ));
    }

    #[test]
    fn confirm_taker_lock_errors_before_record_taker_lock() {
        let run_id = crate::domain::types::RuntimeRunId::generate();
        let settlement = TwoSidedSettlement::new(terms());

        let error = settlement
            .confirm_taker_lock(run_id)
            .expect_err("confirm_taker_lock without record_taker_lock should error");
        assert!(error.to_string().contains("confirm_taker_lock"));
    }

    #[test]
    fn confirm_maker_lock_errors_before_record_maker_lock() {
        let run_id = crate::domain::types::RuntimeRunId::generate();
        let mut settlement = TwoSidedSettlement::new(terms());
        // Advance through taker phases without locking maker output.
        let _ = settlement.record_taker_lock(run_id, Some("sig".into()));
        let _ = settlement.confirm_taker_lock(run_id).expect("taker confirm");

        let error = settlement
            .confirm_maker_lock(run_id)
            .expect_err("confirm_maker_lock without record_maker_lock should error");
        assert!(error.to_string().contains("confirm_maker_lock"));
    }

    #[test]
    fn two_sided_state_emits_refund_and_failed_events() {
        let run_id = crate::domain::types::RuntimeRunId::generate();
        let terms = terms();
        let mut settlement = TwoSidedSettlement::new(terms.clone());

        let refund =
            settlement.record_refund(run_id, SettlementLeg::TakerInput, Some("refund-sig".into()));
        assert_eq!(settlement.phase, SettlementPhase::Refunded);
        assert!(matches!(
            refund.step,
            SettlementStep::Refund(SettlementLeg::TakerInput)
        ));
        assert!(matches!(
            refund.event,
            crate::domain::events::SettlementEvent::Refunded { .. }
        ));

        let failure = settlement.fail(run_id, "maker lock validation failed");
        assert_eq!(settlement.phase, SettlementPhase::Failed);
        assert!(matches!(failure.step, SettlementStep::Failed));
        assert!(matches!(
            failure.event,
            crate::domain::events::SettlementEvent::Failed { trade_id, ref reason, .. }
                if trade_id == terms.trade_id && reason == "maker lock validation failed"
        ));
    }
}
