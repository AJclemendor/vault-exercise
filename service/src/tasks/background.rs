use crate::AppState;
use crate::stats::pct;
use std::time::Duration;

const ACTIVE_REFRESH_INTERVAL: Duration = Duration::from_millis(300);
const LOG_POLL_INTERVAL: Duration = Duration::from_millis(250);
const STATS_LOG_INTERVAL: Duration = Duration::from_secs(5);
const ACTIVE_REFRESH_BUDGET: usize = 40;
const BAR_WIDTH: usize = 24;
const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_CYAN: &str = "\x1b[36m";

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
            paint(snapshot.orders_rejected_insufficient_balance, rejected_color),
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
