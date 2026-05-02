use super::{PendingSettlement, PostSubmitFailurePolicy, PreSubmitDecision, SubmitOutcome};
use crate::AppState;
use crate::chain::{SettlementConfirmationError, SettlementReceiptStatus};
use crate::engine::{Engine, FillCandidate};
use crate::runtime::receipt_tuning;
use crate::sequencing::OrderedGate;
use alloy::primitives::TxHash;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;

pub(super) async fn process_fill(state: &AppState, fill: &FillCandidate) {
    if prepare_fill_for_submit(state, fill).await == PreSubmitDecision::Abort {
        return;
    }

    let SubmitOutcome::Submitted(pending) = submit_settlement_once(state, fill).await else {
        return;
    };

    confirm_and_apply_settlement(
        state.clone(),
        fill.clone(),
        pending,
        None,
        PostSubmitFailurePolicy::ReleaseOrPrune,
    )
    .await;
}

pub(super) async fn prepare_fill_for_submit(
    state: &AppState,
    fill: &FillCandidate,
) -> PreSubmitDecision {
    {
        let mut engine = state.engine.lock().await;
        if !engine.fill_still_pending(fill) {
            engine.record_settlement_aborted_before_tx();
            return PreSubmitDecision::Abort;
        }
        engine.record_settlement_attempted();
    }

    if let Err(err) = refresh_for_settlement(state, fill).await {
        eprintln!(
            "[settlement] refresh failed seq={} buy={} sell={} price={} size={}: {err:#}",
            fill.seq, fill.buyer, fill.seller, fill.exec_price, fill.fill_size
        );
        let mut engine = state.engine.lock().await;
        engine.record_settlement_precheck_failed();
        engine.mark_dirty(fill.buyer);
        engine.mark_dirty(fill.seller);
        engine.abort_fill(fill, false, false);
        return PreSubmitDecision::Abort;
    }

    {
        let mut engine = state.engine.lock().await;
        if !engine.fill_still_pending(fill) {
            engine.record_settlement_aborted_before_tx();
            return PreSubmitDecision::Abort;
        }
        let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
        if !buyer_ok || !seller_ok {
            engine.record_settlement_precheck_failed();
            engine.abort_fill(fill, !buyer_ok, !seller_ok);
            return PreSubmitDecision::Abort;
        }
        engine.record_settlement_tx_attempt();
    }

    PreSubmitDecision::Submit
}

pub(super) async fn submit_settlement_once(
    state: &AppState,
    fill: &FillCandidate,
) -> SubmitOutcome {
    match state
        .chain
        .submit_settlement(fill.buyer, fill.seller, fill.quote, fill.base)
        .await
    {
        Ok(pending) => SubmitOutcome::Submitted(pending),
        Err(err) => match settlement_send_failure_action() {
            SettlementFailureAction::AbortKnownFailure => {
                eprintln!(
                    "[settlement] matchOrders send failed; releasing fill seq={} buy={} sell={} quote={} base={}: {err:#}",
                    fill.seq, fill.buyer, fill.seller, fill.quote, fill.base
                );
                let mut engine = state.engine.lock().await;
                engine.record_settlement_send_failed();
                engine.mark_dirty(fill.buyer);
                engine.mark_dirty(fill.seller);
                engine.abort_fill(fill, true, true);
                SubmitOutcome::SendFailed
            }
            SettlementFailureAction::HoldUncertainOutcome => {
                unreachable!("send failures cannot be held without a transaction hash")
            }
        },
    }
}

pub(super) async fn confirm_and_apply_settlement(
    state: AppState,
    fill: FillCandidate,
    pending: PendingSettlement,
    apply_gate: Option<Arc<OrderedGate>>,
    post_submit_failure_policy: PostSubmitFailurePolicy,
) {
    let tx_hash = *pending.tx_hash();
    let result = state.chain.confirm_settlement(pending).await;

    if let Some(gate) = apply_gate.as_ref() {
        let _turn = gate.wait_for_turn(fill.seq).await;
        apply_settlement_confirmation_result(
            &state,
            &fill,
            tx_hash,
            result,
            post_submit_failure_policy,
        )
        .await;
    } else {
        apply_settlement_confirmation_result(
            &state,
            &fill,
            tx_hash,
            result,
            post_submit_failure_policy,
        )
        .await;
    }
}

async fn apply_settlement_confirmation_result(
    state: &AppState,
    fill: &FillCandidate,
    tx_hash: TxHash,
    result: std::result::Result<(), SettlementConfirmationError>,
    post_submit_failure_policy: PostSubmitFailurePolicy,
) {
    if let Err(err) = result {
        match settlement_confirmation_failure_action(&err) {
            SettlementFailureAction::HoldUncertainOutcome => {
                eprintln!(
                    "[settlement] matchOrders confirmation uncertain; rechecking receipt seq={} tx={} buy={} sell={} quote={} base={}: {err:#}",
                    fill.seq, tx_hash, fill.buyer, fill.seller, fill.quote, fill.base
                );
                match resolve_uncertain_settlement(state, fill, tx_hash).await {
                    UncertainSettlementResolution::Succeeded => {
                        apply_confirmed_settlement_success(state, fill).await;
                        return;
                    }
                    UncertainSettlementResolution::Reverted => {
                        abort_confirmed_reverted_settlement_with_policy(
                            state,
                            fill,
                            post_submit_failure_policy,
                        )
                        .await;
                        return;
                    }
                    UncertainSettlementResolution::Unresolved => {
                        {
                            let mut engine = state.engine.lock().await;
                            hold_unresolved_settlement(&mut engine, fill, &err);
                        }
                        spawn_uncertain_settlement_reconciler(state.clone(), fill.clone(), tx_hash);
                        return;
                    }
                }
            }
            SettlementFailureAction::AbortKnownFailure => {
                eprintln!(
                    "[settlement] matchOrders confirmation failed seq={} tx={} buy={} sell={} quote={} base={}: {err:#}",
                    fill.seq, tx_hash, fill.buyer, fill.seller, fill.quote, fill.base
                );

                abort_confirmed_reverted_settlement_with_policy(
                    state,
                    fill,
                    post_submit_failure_policy,
                )
                .await;
                return;
            }
        }
    }

    apply_confirmed_settlement_success(state, fill).await;
}

pub(super) fn spawn_receipt_task(
    state: AppState,
    fill: FillCandidate,
    pending: PendingSettlement,
    apply_gate: Arc<OrderedGate>,
    receipt_permit: OwnedSemaphorePermit,
    unresolved_permit: OwnedSemaphorePermit,
) {
    tokio::spawn(async move {
        let _receipt_permit = receipt_permit;
        let _unresolved_permit = unresolved_permit;
        confirm_and_apply_settlement(
            state,
            fill,
            pending,
            Some(apply_gate),
            PostSubmitFailurePolicy::StaleBothOrders,
        )
        .await;
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UncertainSettlementResolution {
    Succeeded,
    Reverted,
    Unresolved,
}

async fn resolve_uncertain_settlement(
    state: &AppState,
    fill: &FillCandidate,
    tx_hash: TxHash,
) -> UncertainSettlementResolution {
    let mut last_error = None;
    let tuning = receipt_tuning();

    for attempt in 1..=tuning.uncertain_rechecks {
        tokio::time::sleep(tuning.uncertain_recheck_interval).await;
        match state.chain.settlement_receipt_status(tx_hash).await {
            Ok(Some(SettlementReceiptStatus::Succeeded)) => {
                eprintln!(
                    "[settlement] matchOrders receipt resolved success seq={} tx={} attempt={}/{}",
                    fill.seq, tx_hash, attempt, tuning.uncertain_rechecks
                );
                return UncertainSettlementResolution::Succeeded;
            }
            Ok(Some(SettlementReceiptStatus::Reverted)) => {
                eprintln!(
                    "[settlement] matchOrders receipt resolved revert seq={} tx={} attempt={}/{}",
                    fill.seq, tx_hash, attempt, tuning.uncertain_rechecks
                );
                return UncertainSettlementResolution::Reverted;
            }
            Ok(None) => {}
            Err(err) => {
                last_error = Some(format!("{err:#}"));
            }
        }
    }

    eprintln!(
        "[settlement] matchOrders receipt unresolved after {} checks seq={} tx={} last_error={}",
        tuning.uncertain_rechecks,
        fill.seq,
        tx_hash,
        last_error.unwrap_or_else(|| "receipt still pending".into())
    );
    UncertainSettlementResolution::Unresolved
}

fn hold_unresolved_settlement(
    engine: &mut Engine,
    fill: &FillCandidate,
    _err: &SettlementConfirmationError,
) {
    engine.mark_dirty(fill.buyer);
    engine.mark_dirty(fill.seller);
}

fn spawn_uncertain_settlement_reconciler(state: AppState, fill: FillCandidate, tx_hash: TxHash) {
    tokio::spawn(async move {
        let tuning = receipt_tuning();
        for attempt in 1..=tuning.deferred_rechecks {
            tokio::time::sleep(tuning.deferred_recheck_interval).await;
            match state.chain.settlement_receipt_status(tx_hash).await {
                Ok(Some(SettlementReceiptStatus::Succeeded)) => {
                    eprintln!(
                        "[settlement] deferred receipt resolved success seq={} tx={} attempt={}",
                        fill.seq, tx_hash, attempt
                    );
                    apply_confirmed_settlement_success(&state, &fill).await;
                    return;
                }
                Ok(Some(SettlementReceiptStatus::Reverted)) => {
                    eprintln!(
                        "[settlement] deferred receipt resolved revert seq={} tx={} attempt={}",
                        fill.seq, tx_hash, attempt
                    );
                    abort_confirmed_reverted_settlement(&state, &fill).await;
                    return;
                }
                Ok(None) => {}
                Err(err) => {
                    if attempt == 1 || attempt == tuning.deferred_rechecks {
                        eprintln!(
                            "[settlement] deferred receipt still unresolved seq={} tx={} attempt={}: {err:#}",
                            fill.seq, tx_hash, attempt
                        );
                    }
                }
            }
        }

        eprintln!(
            "[settlement] deferred receipt timed out seq={} tx={} checks={}; staling both orders",
            fill.seq, tx_hash, tuning.deferred_rechecks
        );
        let mut engine = state.engine.lock().await;
        time_out_unresolved_settlement(&mut engine, &fill);
    });
}

fn time_out_unresolved_settlement(engine: &mut Engine, fill: &FillCandidate) {
    engine.record_settlement_receipt_failed();
    engine.record_settlement_unknown_outcome();
    engine.mark_dirty(fill.buyer);
    engine.mark_dirty(fill.seller);
    engine.abort_fill(fill, true, true);
}

async fn apply_confirmed_settlement_success(state: &AppState, fill: &FillCandidate) {
    if let Err(err) = refresh_after_success(state, fill).await {
        eprintln!(
            "[settlement] post-success refresh failed seq={} buy={} sell={} quote={} base={}: {err:#}",
            fill.seq, fill.buyer, fill.seller, fill.quote, fill.base
        );
        let mut engine = state.engine.lock().await;
        engine.mark_dirty(fill.buyer);
        engine.mark_dirty(fill.seller);
    }
    let mut engine = state.engine.lock().await;
    engine.apply_settlement_success(fill);
}

async fn abort_confirmed_reverted_settlement(state: &AppState, fill: &FillCandidate) {
    abort_confirmed_reverted_settlement_with_policy(
        state,
        fill,
        PostSubmitFailurePolicy::ReleaseOrPrune,
    )
    .await;
}

async fn abort_confirmed_reverted_settlement_with_policy(
    state: &AppState,
    fill: &FillCandidate,
    policy: PostSubmitFailurePolicy,
) {
    let funded_after_failure = refresh_after_failed_settlement(state, fill).await;
    let mut engine = state.engine.lock().await;
    engine.record_settlement_reverted();
    if funded_after_failure.is_err() {
        engine.mark_dirty(fill.buyer);
        engine.mark_dirty(fill.seller);
    }
    match policy {
        PostSubmitFailurePolicy::ReleaseOrPrune => {
            let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
            engine.abort_fill(fill, !buyer_ok, !seller_ok);
        }
        PostSubmitFailurePolicy::StaleBothOrders => {
            engine.abort_fill(fill, true, true);
        }
    }
}

pub(super) async fn refresh_for_settlement(state: &AppState, fill: &FillCandidate) -> Result<()> {
    let (buyer_balances, seller_balances, refresh_count) = if fill.seller == fill.buyer {
        let balances = state.chain.read_user_balances(fill.buyer).await?;
        (balances, balances, 1)
    } else {
        let (buyer_balances, seller_balances) = tokio::try_join!(
            state.chain.read_user_balances(fill.buyer),
            state.chain.read_user_balances(fill.seller)
        )?;
        (buyer_balances, seller_balances, 2)
    };

    let mut engine = state.engine.lock().await;
    engine.apply_balance_refresh_at_block(
        fill.buyer,
        buyer_balances.real,
        buyer_balances.vault,
        buyer_balances.block,
    );
    if fill.seller != fill.buyer {
        engine.apply_balance_refresh_at_block(
            fill.seller,
            seller_balances.real,
            seller_balances.vault,
            seller_balances.block,
        );
    }
    engine.record_pre_settlement_balance_refreshes(refresh_count);
    Ok(())
}

async fn refresh_after_failed_settlement(state: &AppState, fill: &FillCandidate) -> Result<bool> {
    refresh_for_settlement(state, fill).await?;
    let engine = state.engine.lock().await;
    let (buyer_ok, seller_ok) = engine.users_funded_for_reserved(fill);
    Ok(buyer_ok && seller_ok)
}

fn record_settlement_confirmation_failure(engine: &mut Engine, err: &SettlementConfirmationError) {
    match err {
        SettlementConfirmationError::Reverted => engine.record_settlement_reverted(),
        SettlementConfirmationError::Receipt(_) => engine.record_settlement_receipt_failed(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettlementFailureAction {
    AbortKnownFailure,
    HoldUncertainOutcome,
}

fn settlement_confirmation_failure_action(
    err: &SettlementConfirmationError,
) -> SettlementFailureAction {
    if err.outcome_is_uncertain() {
        SettlementFailureAction::HoldUncertainOutcome
    } else {
        SettlementFailureAction::AbortKnownFailure
    }
}

fn settlement_send_failure_action() -> SettlementFailureAction {
    SettlementFailureAction::AbortKnownFailure
}

async fn refresh_after_success(state: &AppState, fill: &FillCandidate) -> Result<()> {
    let (buyer_balances, seller_balances) = tokio::try_join!(
        state.chain.read_user_balances(fill.buyer),
        state.chain.read_user_balances(fill.seller)
    )?;

    let mut engine = state.engine.lock().await;
    engine.apply_balance_refresh_at_block(
        fill.buyer,
        buyer_balances.real,
        buyer_balances.vault,
        buyer_balances.block,
    );
    if fill.seller != fill.buyer {
        engine.apply_balance_refresh_at_block(
            fill.seller,
            seller_balances.real,
            seller_balances.vault,
            seller_balances.block,
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "settlement_tests.rs"]
mod tests;
