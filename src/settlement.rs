//! Two-sided HTLC settlement state for the maker runtime.
//!
//! The state model is intentionally pure: it creates the two `HtlcInitiation`
//! requests expected by the scaffold `HtlcClient` contract and returns
//! `SettlementEvent`-compatible transitions as receipts arrive. It does not sign
//! transactions, submit RPC calls, or write ledger records.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::events::{EventMetadata, SettlementEvent};
use crate::types::{
    HtlcInitiation, HtlcReceipt, QuoteId, RuntimeRunId, SettlementStatus, TokenAmount, TradeId,
    TxSignature, WalletRole,
};

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
    /// Shared refund deadline used by the scaffold HTLC initiation contract.
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
    /// Taker locks source funds.
    TakerLock,
    /// Maker locks destination funds.
    MakerLock,
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
    /// Runtime event payload for API/TUI/event sinks.
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

    /// Record taker source HTLC initiation.
    #[must_use]
    pub fn record_taker_lock(
        &mut self,
        run_id: RuntimeRunId,
        signature: Option<String>,
    ) -> SettlementTransition {
        self.phase = SettlementPhase::TakerLocked;
        let receipt = self.receipt(SettlementStatus::Initiated, signature);
        self.taker_input_leg.lock_receipt = Some(receipt.clone());
        SettlementTransition {
            step: SettlementStep::TakerLock,
            event: SettlementEvent::Initiated {
                metadata: EventMetadata::new(run_id),
                receipt,
            },
        }
    }

    /// Record maker destination HTLC initiation.
    #[must_use]
    pub fn record_maker_lock(
        &mut self,
        run_id: RuntimeRunId,
        signature: Option<String>,
    ) -> SettlementTransition {
        self.phase = SettlementPhase::MakerLocked;
        let receipt = self.receipt(SettlementStatus::Initiated, signature);
        self.maker_output_leg.lock_receipt = Some(receipt.clone());
        SettlementTransition {
            step: SettlementStep::MakerLock,
            event: SettlementEvent::Initiated {
                metadata: EventMetadata::new(run_id),
                receipt,
            },
        }
    }

    /// Record taker redemption of maker output.
    #[must_use]
    pub fn record_taker_redeem(
        &mut self,
        run_id: RuntimeRunId,
        signature: Option<String>,
    ) -> SettlementTransition {
        self.phase = SettlementPhase::TakerRedeemed;
        let receipt = self.receipt(SettlementStatus::Redeemed, signature);
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
    #[must_use]
    pub fn record_maker_redeem(
        &mut self,
        run_id: RuntimeRunId,
        signature: Option<String>,
    ) -> SettlementTransition {
        self.phase = SettlementPhase::Complete;
        let receipt = self.receipt(SettlementStatus::Redeemed, signature);
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
        let receipt = self.receipt(SettlementStatus::Refunded, signature);
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

    fn receipt(&self, status: SettlementStatus, signature: Option<String>) -> HtlcReceipt {
        HtlcReceipt {
            trade_id: self.terms.trade_id,
            status,
            signature: signature.map(TxSignature::new),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AmountRaw, AssetId, QuoteId, TokenAmount, TradeId, WalletRole};
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
        let run_id = crate::types::RuntimeRunId::generate();
        let terms = terms();
        let mut settlement = TwoSidedSettlement::new(terms.clone());

        let started = settlement.start(run_id);
        assert_eq!(settlement.phase, SettlementPhase::Started);
        assert!(matches!(
            started.event,
            crate::events::SettlementEvent::Started { trade_id, quote_id, .. }
                if trade_id == terms.trade_id && quote_id == terms.quote_id
        ));

        let taker_lock = settlement.record_taker_lock(run_id, Some("taker-lock-sig".into()));
        assert_eq!(settlement.phase, SettlementPhase::TakerLocked);
        assert!(matches!(taker_lock.step, SettlementStep::TakerLock));
        assert!(matches!(
            taker_lock.event,
            crate::events::SettlementEvent::Initiated { .. }
        ));

        let maker_lock = settlement.record_maker_lock(run_id, Some("maker-lock-sig".into()));
        assert_eq!(settlement.phase, SettlementPhase::MakerLocked);
        assert!(matches!(maker_lock.step, SettlementStep::MakerLock));
        assert!(matches!(
            maker_lock.event,
            crate::events::SettlementEvent::Initiated { .. }
        ));

        let taker_redeem = settlement.record_taker_redeem(run_id, Some("taker-redeem-sig".into()));
        assert_eq!(settlement.phase, SettlementPhase::TakerRedeemed);
        assert!(matches!(taker_redeem.step, SettlementStep::TakerRedeem));
        assert!(matches!(
            taker_redeem.event,
            crate::events::SettlementEvent::Redeemed { .. }
        ));

        let maker_redeem = settlement.record_maker_redeem(run_id, Some("maker-redeem-sig".into()));
        assert_eq!(settlement.phase, SettlementPhase::Complete);
        assert!(matches!(maker_redeem.step, SettlementStep::MakerRedeem));
        assert!(matches!(
            maker_redeem.event,
            crate::events::SettlementEvent::Redeemed { .. }
        ));
    }

    #[test]
    fn two_sided_state_emits_refund_and_failed_events() {
        let run_id = crate::types::RuntimeRunId::generate();
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
            crate::events::SettlementEvent::Refunded { .. }
        ));

        let failure = settlement.fail(run_id, "maker lock validation failed");
        assert_eq!(settlement.phase, SettlementPhase::Failed);
        assert!(matches!(failure.step, SettlementStep::Failed));
        assert!(matches!(
            failure.event,
            crate::events::SettlementEvent::Failed { trade_id, ref reason, .. }
                if trade_id == terms.trade_id && reason == "maker lock validation failed"
        ));
    }
}
