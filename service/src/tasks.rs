mod settlement;

use crate::AppState;
use crate::runtime::task_tuning;

pub(crate) use settlement::settlement_loop;

const BAR_WIDTH: usize = 24;
const ANSI_RESET: &str = "\x1b[0m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_CYAN: &str = "\x1b[36m";

pub(crate) async fn active_refresh_loop(state: AppState) {
    let tuning = task_tuning();
    loop {
        tokio::time::sleep(tuning.active_refresh_interval).await;
        let candidates = {
            let engine = state.engine.lock().await;
            engine.refresh_candidates(tuning.active_refresh_budget)
        };

        for user in candidates {
            match state.chain.read_user_balances(user).await {
                Ok(balance) => {
                    let mut engine = state.engine.lock().await;
                    engine.apply_balance_refresh_at_block(
                        user,
                        balance.real,
                        balance.vault,
                        balance.block,
                    );
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
    let tuning = task_tuning();
    let mut last_seen = match state.chain.block_number().await {
        Ok(block) => block,
        Err(err) => {
            eprintln!("[logs] initial block query failed: {err:#}");
            0
        }
    };

    loop {
        tokio::time::sleep(tuning.log_poll_interval).await;
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
            Ok(events) => {
                if !events.is_empty() {
                    let mut engine = state.engine.lock().await;
                    for event in events {
                        engine.mark_dirty_at_block(event.user, event.block);
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
    let tuning = task_tuning();
    loop {
        tokio::time::sleep(tuning.stats_log_interval).await;
        let snapshot = {
            let engine = state.engine.lock().await;
            engine.stats_snapshot()
        };
        let rejected_color = nonzero_color(snapshot.orders_rejected, ANSI_YELLOW);
        let revert_color = nonzero_color(snapshot.settlements_reverted, ANSI_RED);
        let settlement_unattempted = snapshot
            .fill_candidates
            .saturating_sub(snapshot.settlements_attempted);
        let settlement_pending_attempted = snapshot
            .settlement_pending_outcomes
            .saturating_sub(settlement_unattempted);
        let settlement_pending_color = nonzero_color(settlement_pending_attempted, ANSI_YELLOW);
        let settlement_unattempted_color = nonzero_color(settlement_unattempted, ANSI_YELLOW);

        println!(
            concat!(
                "\n{}[stats]{}\n",
                "  orders_received       {}\n",
                "  orders_accepted       {}/{} {} {}\n",
                "  orders_rejected       {}/{} {} {} (admission_failures={} bad_req={} insuff={} stale_cache={} refresh_fail={})\n",
                "  orders_matched        {}/{} {} {} (unique orders with >=1 successful fill; fill_side_events={})\n",
                "  settlements_attempted {}/{} {} of candidates (fill_candidates={} precheck_passed={}/{} {} precheck_failed={}/{} {} tx_attempts={}/{} {} tx_submitted={}/{} {})\n",
                "  settlements_reverted  {} {} of tx_attempts (receipt_status_reverted={})\n",
                "  settlement_outcomes   success={} reverted={} send_fail={} receipt_fail={} precheck_fail={} aborted_before_tx={} unknown={} pending={} unattempted={}\n",
                "  currently_open_orders {} live (open_status={} partial_status={}; lifetime_accepted_pct={})",
            ),
            ANSI_CYAN,
            ANSI_RESET,
            stat_count(snapshot.orders_received, ANSI_DIM),
            stat_count(snapshot.orders_accepted, ANSI_GREEN),
            paint(snapshot.orders_received, ANSI_DIM),
            stat_pct(snapshot.orders_accepted_pct, ANSI_GREEN),
            pct_bar_colored(snapshot.orders_accepted_pct, ANSI_GREEN),
            stat_count(snapshot.orders_rejected, rejected_color),
            paint(snapshot.orders_received, ANSI_DIM),
            stat_pct(snapshot.orders_rejected_pct, rejected_color),
            pct_bar_colored(snapshot.orders_rejected_pct, rejected_color),
            paint(snapshot.orders_admission_failures, rejected_color),
            paint(snapshot.orders_rejected_bad_request, rejected_color),
            paint(
                snapshot.orders_rejected_insufficient_balance,
                rejected_color
            ),
            paint(snapshot.orders_rejected_stale_balance_cache, rejected_color),
            paint(snapshot.orders_failed_balance_refresh, rejected_color),
            stat_count(snapshot.orders_matched, ANSI_GREEN),
            paint(snapshot.orders_accepted, ANSI_DIM),
            stat_pct(snapshot.orders_matched_pct_of_accepted, ANSI_GREEN),
            pct_bar_colored(snapshot.orders_matched_pct_of_accepted, ANSI_GREEN),
            paint(snapshot.order_fill_side_events, ANSI_CYAN),
            stat_count(snapshot.settlements_attempted, ANSI_DIM),
            paint(snapshot.fill_candidates, ANSI_CYAN),
            stat_pct(snapshot.settlements_attempted_pct_of_candidates, ANSI_DIM),
            paint(snapshot.fill_candidates, ANSI_CYAN),
            paint(snapshot.settlement_precheck_passed, ANSI_GREEN),
            paint(snapshot.settlement_precheck_attempts, ANSI_DIM),
            stat_pct(snapshot.settlement_precheck_passed_pct, ANSI_GREEN),
            paint(snapshot.settlements_precheck_failed, rejected_color),
            paint(snapshot.settlement_precheck_attempts, ANSI_DIM),
            stat_pct(snapshot.settlements_precheck_failed_pct, rejected_color),
            paint(snapshot.settlement_tx_attempts, ANSI_DIM),
            paint(snapshot.settlement_precheck_passed, ANSI_DIM),
            stat_pct(
                snapshot.settlement_tx_attempts_pct_of_precheck_passed,
                ANSI_DIM
            ),
            paint(snapshot.settlement_tx_submitted, ANSI_GREEN),
            paint(snapshot.settlement_tx_attempts, ANSI_DIM),
            stat_pct(snapshot.settlement_tx_submitted_pct_of_attempts, ANSI_GREEN),
            stat_count(snapshot.settlements_reverted, revert_color),
            stat_pct(snapshot.settlements_reverted_pct, revert_color),
            paint(snapshot.settlement_receipt_status_reverted, revert_color),
            paint(snapshot.successful_settlements, ANSI_GREEN),
            paint(snapshot.settlements_reverted, revert_color),
            paint(snapshot.settlement_send_failures, rejected_color),
            paint(snapshot.settlement_receipt_failures, rejected_color),
            paint(snapshot.settlements_precheck_failed, rejected_color),
            paint(snapshot.settlements_aborted_before_tx, ANSI_YELLOW),
            paint(snapshot.settlement_unknown_outcomes, ANSI_YELLOW),
            paint(settlement_pending_attempted, settlement_pending_color),
            paint(settlement_unattempted, settlement_unattempted_color),
            stat_count(snapshot.currently_open_orders as u64, ANSI_CYAN),
            paint(snapshot.currently_open_status_orders, ANSI_CYAN),
            paint(snapshot.currently_partially_filled_orders, ANSI_CYAN),
            stat_pct(snapshot.currently_open_orders_pct_of_accepted, ANSI_DIM),
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
