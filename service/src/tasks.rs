use crate::AppState;
use crate::chain::{SettlementConfirmationError, SettlementReceiptStatus};
use crate::engine::{Engine, FillCandidate};
use crate::stats::pct;
use alloy::primitives::TxHash;
use anyhow::Result;
use std::time::Duration;

const ACTIVE_REFRESH_INTERVAL: Duration = Duration::from_millis(300);
const LOG_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MATCH_IDLE_SLEEP: Duration = Duration::from_millis(25);
const STATS_LOG_INTERVAL: Duration = Duration::from_secs(5);
const ACTIVE_REFRESH_BUDGET: usize = 40;
const BAR_WIDTH: usize = 24;
const UNCERTAIN_RECEIPT_RECHECKS: usize = 20;
const UNCERTAIN_RECEIPT_RECHECK_INTERVAL: Duration = Duration::from_millis(250);
const DEFERRED_RECEIPT_RECHECK_INTERVAL: Duration = Duration::from_secs(1);
const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_CYAN: &str = "\x1b[36m";

pub(crate) async fn settlement_loop(state: AppState) {
    println!("[config] settlement_mode=sequential settlement_concurrency=1");
    sequential_settlement_loop(state).await;
}

async fn sequential_settlement_loop(state: AppState) {
    loop {
        let fill = {
            let mut engine = state.engine.lock().await;
            engine.next_fill_candidate()
        };
        let Some(fill) = fill else {
            tokio::time::sleep(MATCH_IDLE_SLEEP).await;
            continue;
        };

        process_fill(&state, &fill).await;
    }
}

async fn process_fill(state: &AppState, fill: &FillCandidate) {
    {
        let mut engine = state.engine.lock().await;
        if !engine.fill_still_pending(fill) {
            return;
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
        return;
    }

    {
        let mut engine = state.engine.lock().await;
        if !engine.fill_still_pending(fill) {
            return;
        }
        let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
        if !buyer_ok || !seller_ok {
            engine.record_settlement_precheck_failed();
            engine.abort_fill(fill, !buyer_ok, !seller_ok);
            return;
        }
    }

    {
        let mut engine = state.engine.lock().await;
        if !engine.fill_still_pending(fill) {
            return;
        }
        let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
        if !buyer_ok || !seller_ok {
            engine.record_settlement_precheck_failed();
            engine.abort_fill(fill, !buyer_ok, !seller_ok);
            return;
        }
        engine.record_settlement_tx_attempt();
    }

    match state
        .chain
        .submit_settlement(fill.buyer, fill.seller, fill.quote, fill.base)
        .await
    {
        Ok(pending) => {
            let tx_hash = *pending.tx_hash();
            let result = state.chain.confirm_settlement(pending).await;
            if let Err(err) = result {
                match settlement_confirmation_failure_action(&err) {
                    SettlementFailureAction::HoldUncertainOutcome => {
                        eprintln!(
                            "[settlement] matchOrders confirmation uncertain; rechecking receipt seq={} tx={} buy={} sell={} quote={} base={}: {err:#}",
                            fill.seq, tx_hash, fill.buyer, fill.seller, fill.quote, fill.base
                        );
                        match resolve_uncertain_settlement(state, fill, tx_hash).await {
                            UncertainSettlementResolution::Succeeded => return,
                            UncertainSettlementResolution::Reverted => {
                                let funded_after_failure =
                                    refresh_after_failed_settlement(state, fill).await;
                                let mut engine = state.engine.lock().await;
                                engine.record_settlement_reverted();
                                if funded_after_failure.is_err() {
                                    engine.mark_dirty(fill.buyer);
                                    engine.mark_dirty(fill.seller);
                                }
                                let (buyer_ok, seller_ok) =
                                    engine.prune_underfunded_fill_users(fill);
                                engine.abort_fill(fill, !buyer_ok, !seller_ok);
                                return;
                            }
                            UncertainSettlementResolution::Unresolved => {
                                {
                                    let mut engine = state.engine.lock().await;
                                    hold_unresolved_settlement(&mut engine, fill, &err);
                                }
                                spawn_uncertain_settlement_reconciler(
                                    state.clone(),
                                    fill.clone(),
                                    tx_hash,
                                );
                                return;
                            }
                        }
                    }
                    SettlementFailureAction::AbortKnownFailure => {
                        eprintln!(
                            "[settlement] matchOrders confirmation failed seq={} tx={} buy={} sell={} quote={} base={}: {err:#}",
                            fill.seq, tx_hash, fill.buyer, fill.seller, fill.quote, fill.base
                        );

                        let funded_after_failure =
                            refresh_after_failed_settlement(state, fill).await;
                        let mut engine = state.engine.lock().await;
                        record_settlement_confirmation_failure(&mut engine, &err);
                        if funded_after_failure.is_err() {
                            engine.mark_dirty(fill.buyer);
                            engine.mark_dirty(fill.seller);
                        }
                        let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
                        engine.abort_fill(fill, !buyer_ok, !seller_ok);
                        return;
                    }
                }
            }

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
                engine.abort_fill(fill, false, false);
            }
            SettlementFailureAction::HoldUncertainOutcome => {
                unreachable!("send failures cannot be held without a transaction hash")
            }
        },
    }
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

    for attempt in 1..=UNCERTAIN_RECEIPT_RECHECKS {
        tokio::time::sleep(UNCERTAIN_RECEIPT_RECHECK_INTERVAL).await;
        match state.chain.settlement_receipt_status(tx_hash).await {
            Ok(Some(SettlementReceiptStatus::Succeeded)) => {
                eprintln!(
                    "[settlement] matchOrders receipt resolved success seq={} tx={} attempt={}/{}",
                    fill.seq, tx_hash, attempt, UNCERTAIN_RECEIPT_RECHECKS
                );
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
                return UncertainSettlementResolution::Succeeded;
            }
            Ok(Some(SettlementReceiptStatus::Reverted)) => {
                eprintln!(
                    "[settlement] matchOrders receipt resolved revert seq={} tx={} attempt={}/{}",
                    fill.seq, tx_hash, attempt, UNCERTAIN_RECEIPT_RECHECKS
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
        UNCERTAIN_RECEIPT_RECHECKS,
        fill.seq,
        tx_hash,
        last_error.unwrap_or_else(|| "receipt still pending".into())
    );
    UncertainSettlementResolution::Unresolved
}

fn hold_unresolved_settlement(
    engine: &mut Engine,
    fill: &FillCandidate,
    err: &SettlementConfirmationError,
) {
    record_settlement_confirmation_failure(engine, err);
    engine.record_settlement_unknown_outcome();
    engine.mark_dirty(fill.buyer);
    engine.mark_dirty(fill.seller);
}

fn spawn_uncertain_settlement_reconciler(state: AppState, fill: FillCandidate, tx_hash: TxHash) {
    tokio::spawn(async move {
        let mut attempts = 0usize;

        loop {
            attempts += 1;
            tokio::time::sleep(DEFERRED_RECEIPT_RECHECK_INTERVAL).await;
            match state.chain.settlement_receipt_status(tx_hash).await {
                Ok(Some(SettlementReceiptStatus::Succeeded)) => {
                    eprintln!(
                        "[settlement] deferred receipt resolved success seq={} tx={} attempt={}",
                        fill.seq, tx_hash, attempts
                    );
                    apply_confirmed_settlement_success(&state, &fill).await;
                    return;
                }
                Ok(Some(SettlementReceiptStatus::Reverted)) => {
                    eprintln!(
                        "[settlement] deferred receipt resolved revert seq={} tx={} attempt={}",
                        fill.seq, tx_hash, attempts
                    );
                    abort_confirmed_reverted_settlement(&state, &fill).await;
                    return;
                }
                Ok(None) => {}
                Err(err) => {
                    if attempts == 1 || attempts % 30 == 0 {
                        eprintln!(
                            "[settlement] deferred receipt still unresolved seq={} tx={} attempt={}: {err:#}",
                            fill.seq, tx_hash, attempts
                        );
                    }
                }
            }
        }
    });
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
    let funded_after_failure = refresh_after_failed_settlement(state, fill).await;
    let mut engine = state.engine.lock().await;
    engine.record_settlement_reverted();
    if funded_after_failure.is_err() {
        engine.mark_dirty(fill.buyer);
        engine.mark_dirty(fill.seller);
    }
    let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
    engine.abort_fill(fill, !buyer_ok, !seller_ok);
}

async fn refresh_for_settlement(state: &AppState, fill: &FillCandidate) -> Result<()> {
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
    engine.apply_balance_refresh(fill.buyer, buyer_balances.0, buyer_balances.1);
    if fill.seller != fill.buyer {
        engine.apply_balance_refresh(fill.seller, seller_balances.0, seller_balances.1);
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
    engine.apply_balance_refresh(fill.buyer, buyer_balances.0, buyer_balances.1);
    if fill.seller != fill.buyer {
        engine.apply_balance_refresh(fill.seller, seller_balances.0, seller_balances.1);
    }
    Ok(())
}

pub(crate) async fn active_refresh_loop(state: AppState) {
    loop {
        tokio::time::sleep(ACTIVE_REFRESH_INTERVAL).await;
        let candidates = {
            let engine = state.engine.lock().await;
            engine.refresh_candidates(ACTIVE_REFRESH_BUDGET)
        };

        for user in candidates {
            match state.chain.read_user_balances(user).await {
                Ok((real, vault)) => {
                    let mut engine = state.engine.lock().await;
                    engine.apply_balance_refresh(user, real, vault);
                    engine.prune_user_to_balance(user, None);
                    engine.record_background_balance_refresh();
                }
                Err(err) => {
                    eprintln!("[balance] background refresh failed user={user}: {err:#}");
                    let mut engine = state.engine.lock().await;
                    engine.mark_dirty(user);
                }
            }
        }
    }
}

pub(crate) async fn log_poll_loop(state: AppState) {
    let mut last_seen = match state.chain.block_number().await {
        Ok(block) => block,
        Err(err) => {
            eprintln!("[logs] initial block query failed: {err:#}");
            0
        }
    };

    loop {
        tokio::time::sleep(LOG_POLL_INTERVAL).await;
        let latest = match state.chain.block_number().await {
            Ok(block) => block,
            Err(err) => {
                eprintln!("[logs] block query failed: {err:#}");
                continue;
            }
        };

        if latest <= last_seen {
            continue;
        }

        let from = last_seen + 1;
        let to = latest;
        match state.chain.dirty_users_from_logs(from, to).await {
            Ok(users) => {
                if !users.is_empty() {
                    let mut engine = state.engine.lock().await;
                    for user in users {
                        engine.mark_dirty(user);
                    }
                }
                last_seen = to;
            }
            Err(err) => {
                eprintln!("[logs] getLogs failed from={from} to={to}: {err:#}");
            }
        }
    }
}

pub(crate) async fn stats_log_loop(state: AppState) {
    loop {
        tokio::time::sleep(STATS_LOG_INTERVAL).await;
        let snapshot = {
            let engine = state.engine.lock().await;
            engine.stats_snapshot()
        };
        let fill_candidates_pct_of_accepted =
            pct(snapshot.fill_candidates, snapshot.orders_accepted);
        let rejected_color = nonzero_color(snapshot.orders_rejected, ANSI_YELLOW);
        let admission_failure_color =
            nonzero_color(snapshot.orders_admission_failures, ANSI_YELLOW);
        let settlement_failure_color = nonzero_color(snapshot.settlement_failures, ANSI_RED);
        let precheck_color = nonzero_color(snapshot.settlements_precheck_failed, ANSI_YELLOW);
        let tx_failure_color = nonzero_color(snapshot.settlement_tx_failures, ANSI_RED);
        let stale_color = nonzero_color(snapshot.currently_stale_orders as u64, ANSI_YELLOW);
        let dirty_color = nonzero_color(snapshot.cache_dirty_events, ANSI_YELLOW);

        println!(
            concat!(
                "\n{}[stats]{}\n",
                "  {}orders{}\n",
                "    received   {}\n",
                "    accepted   {} {} {}\n",
                "    rejected   {} {} {}\n",
                "      admission_failures {} {} bad_req={} insuff={} stale_cache={} refresh_fail={}\n",
                "    matched    {} {} of accepted {} (settled order sides)\n",
                "    unique_filled_orders {}\n",
                "    market_ioc accepted={} open={} cancelled_unfilled={}\n",
                "    open       {} {} of accepted {}\n",
                "  {}settlements{}\n",
                "    attempted  {}\n",
                "    failures   {} {} precheck={} tx_fail={} send={} receipt={} revert={} unknown={}\n",
                "  {}diagnostics{}\n",
                "    fill_candidates {} {} of accepted {}\n",
                "    ok              {} {} of attempted, {} of candidates\n",
                "    precheck_failed {} {} of attempted\n",
                "    tx_attempts     {}\n",
                "    status          open={} partial={} filled={} cancelled={} stale={}\n",
                "    refresh         admission={} pre_settlement={} background={} dirty={}",
            ),
            ANSI_CYAN,
            ANSI_RESET,
            ANSI_BOLD,
            ANSI_RESET,
            stat_count(snapshot.orders_received, ANSI_DIM),
            stat_count(snapshot.orders_accepted, ANSI_GREEN),
            stat_pct(snapshot.orders_accepted_pct, ANSI_GREEN),
            pct_bar_colored(snapshot.orders_accepted_pct, ANSI_GREEN),
            stat_count(snapshot.orders_rejected, rejected_color),
            stat_pct(snapshot.orders_rejected_pct, rejected_color),
            pct_bar_colored(snapshot.orders_rejected_pct, rejected_color),
            stat_count(snapshot.orders_admission_failures, admission_failure_color),
            stat_pct(
                snapshot.orders_admission_failures_pct,
                admission_failure_color
            ),
            paint(snapshot.orders_rejected_bad_request, rejected_color),
            paint(
                snapshot.orders_rejected_insufficient_balance,
                rejected_color
            ),
            paint(snapshot.orders_rejected_stale_balance_cache, rejected_color),
            paint(snapshot.orders_failed_balance_refresh, rejected_color),
            stat_count(snapshot.orders_matched, ANSI_GREEN),
            stat_pct(snapshot.orders_matched_pct_of_accepted, ANSI_GREEN),
            pct_bar_colored(snapshot.orders_matched_pct_of_accepted, ANSI_GREEN),
            stat_count(snapshot.unique_orders_with_successful_fill, ANSI_GREEN),
            paint(snapshot.market_ioc_orders_accepted, ANSI_CYAN),
            paint(snapshot.currently_open_market_ioc_orders, ANSI_CYAN),
            paint(snapshot.market_ioc_orders_cancelled_unfilled, ANSI_CYAN),
            stat_count(snapshot.currently_open_orders as u64, ANSI_CYAN),
            stat_pct(snapshot.currently_open_orders_pct_of_accepted, ANSI_CYAN),
            pct_bar_colored(snapshot.currently_open_orders_pct_of_accepted, ANSI_CYAN),
            ANSI_BOLD,
            ANSI_RESET,
            stat_count(snapshot.settlements_attempted, ANSI_DIM),
            stat_count(snapshot.settlement_failures, settlement_failure_color),
            stat_pct(snapshot.settlement_failures_pct, settlement_failure_color),
            paint(snapshot.settlements_precheck_failed, precheck_color),
            paint(snapshot.settlement_tx_failures, tx_failure_color),
            paint(snapshot.settlement_send_failures, tx_failure_color),
            paint(snapshot.settlement_receipt_failures, tx_failure_color),
            paint(snapshot.settlement_reverts, tx_failure_color),
            paint(snapshot.settlement_unknown_outcomes, ANSI_RED),
            ANSI_BOLD,
            ANSI_RESET,
            stat_count(snapshot.fill_candidates, ANSI_CYAN),
            stat_pct(fill_candidates_pct_of_accepted, ANSI_CYAN),
            pct_bar_colored(fill_candidates_pct_of_accepted, ANSI_CYAN),
            stat_count(snapshot.successful_settlements, ANSI_GREEN),
            stat_pct(snapshot.successful_settlements_pct, ANSI_GREEN),
            stat_pct(
                snapshot.successful_settlements_pct_of_candidates,
                ANSI_GREEN
            ),
            stat_count(snapshot.settlements_precheck_failed, precheck_color),
            stat_pct(snapshot.settlements_precheck_failed_pct, precheck_color),
            stat_count(snapshot.settlement_tx_attempts, ANSI_DIM),
            paint(snapshot.currently_open_status_orders, ANSI_CYAN),
            paint(snapshot.currently_partially_filled_orders, ANSI_CYAN),
            paint(snapshot.currently_filled_orders, ANSI_GREEN),
            paint(snapshot.currently_cancelled_orders, ANSI_DIM),
            paint(snapshot.currently_stale_orders, stale_color),
            paint(snapshot.admission_balance_refreshes, ANSI_DIM),
            paint(snapshot.pre_settlement_balance_refreshes, ANSI_DIM),
            paint(snapshot.background_balance_refreshes, ANSI_DIM),
            paint(snapshot.cache_dirty_events, dirty_color),
        );
    }
}

fn nonzero_color(value: u64, nonzero_color: &'static str) -> &'static str {
    if value == 0 { ANSI_DIM } else { nonzero_color }
}

fn stat_count(value: u64, ansi: &str) -> String {
    paint(format!("{value:>8}"), ansi)
}

fn stat_pct(pct: f64, ansi: &str) -> String {
    paint(format!("{pct:>6.1}%"), ansi)
}

fn pct_bar_colored(pct: f64, ansi: &str) -> String {
    paint(pct_bar(pct), ansi)
}

fn paint(text: impl std::fmt::Display, ansi: &str) -> String {
    format!("{ansi}{text}{ANSI_RESET}")
}

fn pct_bar(pct: f64) -> String {
    let filled = ((pct.clamp(0.0, 100.0) / 100.0) * BAR_WIDTH as f64).round() as usize;
    format!(
        "[{}{}]",
        "#".repeat(filled),
        ".".repeat(BAR_WIDTH.saturating_sub(filled))
    )
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
