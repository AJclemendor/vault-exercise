use crate::chain::SettlementConfirmationError;
use crate::engine::{Engine, FillCandidate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettlementFailureAction {
    AbortKnownFailure,
    HoldUncertainOutcome,
}

pub(super) fn settlement_confirmation_failure_action(
    err: &SettlementConfirmationError,
) -> SettlementFailureAction {
    if err.outcome_is_uncertain() {
        SettlementFailureAction::HoldUncertainOutcome
    } else {
        SettlementFailureAction::AbortKnownFailure
    }
}

pub(super) fn settlement_send_failure_action() -> SettlementFailureAction {
    SettlementFailureAction::AbortKnownFailure
}

pub(super) fn abort_release_or_prune_reverted_fill(engine: &mut Engine, fill: &FillCandidate) {
    let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
    let stale_both = buyer_ok && seller_ok;
    engine.abort_fill(fill, !buyer_ok || stale_both, !seller_ok || stale_both);
}
