use crate::AppState;
use crate::chain::SettlementConfirmationError;
use crate::engine::{Engine, FillCandidate};
use anyhow::Result;
use std::time::Duration;

const ACTIVE_REFRESH_INTERVAL: Duration = Duration::from_millis(300);
const LOG_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MATCH_IDLE_SLEEP: Duration = Duration::from_millis(25);
const STATS_LOG_INTERVAL: Duration = Duration::from_secs(5);
const ACTIVE_REFRESH_BUDGET: usize = 40;
const BAR_WIDTH: usize = 24;

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
    if let Err(err) = refresh_for_settlement(state, fill).await {
        eprintln!(
            "[settlement] refresh failed seq={} buy={} sell={} price={} size={}: {err:#}",
            fill.seq, fill.buyer, fill.seller, fill.exec_price, fill.fill_size
        );
        let mut engine = state.engine.lock().await;
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
        engine.record_settlement_attempted();
        let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
        if !buyer_ok || !seller_ok {
            engine.record_settlement_precheck_failed();
            engine.abort_fill(fill, !buyer_ok, !seller_ok);
            return;
        }
    }

    for attempt in 0..=1 {
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
                let result = state.chain.confirm_settlement(pending).await;
                if let Err(err) = result {
                    eprintln!(
                        "[settlement] matchOrders confirmation failed seq={} attempt={} buy={} sell={} quote={} base={}: {err:#}",
                        fill.seq,
                        attempt + 1,
                        fill.buyer,
                        fill.seller,
                        fill.quote,
                        fill.base
                    );

                    let funded_after_failure = refresh_after_failed_settlement(state, fill).await;
                    let mut engine = state.engine.lock().await;
                    record_settlement_confirmation_failure(&mut engine, &err);

                    if attempt == 0
                        && funded_after_failure.unwrap_or(false)
                        && engine.fill_still_pending(fill)
                    {
                        continue;
                    }

                    let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
                    engine.abort_fill(fill, !buyer_ok, !seller_ok);
                    return;
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
                return;
            }
            Err(err) => {
                eprintln!(
                    "[settlement] matchOrders send failed seq={} attempt={} buy={} sell={} quote={} base={}: {err:#}",
                    fill.seq,
                    attempt + 1,
                    fill.buyer,
                    fill.seller,
                    fill.quote,
                    fill.base
                );

                let funded_after_failure = refresh_after_failed_settlement(state, fill).await;
                let mut engine = state.engine.lock().await;
                engine.record_settlement_send_failed();

                if attempt == 0
                    && funded_after_failure.unwrap_or(false)
                    && engine.fill_still_pending(fill)
                {
                    continue;
                }

                let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
                engine.abort_fill(fill, !buyer_ok, !seller_ok);
                return;
            }
        }
    }
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

        println!(
            concat!(
                "\n[stats]\n",
                "  orders\n",
                "    received   {:>8}\n",
                "    accepted   {:>8} {:>6.1}% {}\n",
                "    rejected   {:>8} {:>6.1}% {}\n",
                "      bad_req   {:>8} insuff={:>8} stale_cache={:>8} refresh_fail={:>8}\n",
                "    matched    {:>8} {:>6.1}% of accepted {} (unique orders touched)\n",
                "    fill_sides {:>8}\n",
                "    open       {:>8} {:>6.1}% of accepted {}\n",
                "  settlements\n",
                "    attempted  {:>8}\n",
                "    reverted   {:>8} {:>6.1}% of tx attempts {}\n",
                "    send_fail  {:>8} receipt_fail={:>8}\n",
                "  diagnostics\n",
                "    fill_candidates {:>8} {:>6.1}% of accepted {}\n",
                "    ok              {:>8} {:>6.1}% of attempted, {:>6.1}% of candidates\n",
                "    precheck_failed {:>8} {:>6.1}% of attempted\n",
                "    tx_attempts     {:>8}\n",
                "    status          open={} partial={} filled={} cancelled={} stale={}\n",
                "    refresh         admission={} pre_settlement={} background={} dirty={}",
            ),
            snapshot.orders_received,
            snapshot.orders_accepted,
            snapshot.orders_accepted_pct,
            pct_bar(snapshot.orders_accepted_pct),
            snapshot.orders_rejected,
            snapshot.orders_rejected_pct,
            pct_bar(snapshot.orders_rejected_pct),
            snapshot.orders_rejected_bad_request,
            snapshot.orders_rejected_insufficient_balance,
            snapshot.orders_rejected_stale_balance_cache,
            snapshot.orders_failed_balance_refresh,
            snapshot.orders_matched,
            snapshot.orders_matched_pct_of_accepted,
            pct_bar(snapshot.orders_matched_pct_of_accepted),
            snapshot.order_sides_filled,
            snapshot.currently_open_orders,
            snapshot.currently_open_orders_pct_of_accepted,
            pct_bar(snapshot.currently_open_orders_pct_of_accepted),
            snapshot.settlements_attempted,
            snapshot.settlements_reverted,
            snapshot.settlements_reverted_pct,
            pct_bar(snapshot.settlements_reverted_pct),
            snapshot.settlement_send_failures,
            snapshot.settlement_receipt_failures,
            snapshot.fill_candidates,
            fill_candidates_pct_of_accepted,
            pct_bar(fill_candidates_pct_of_accepted),
            snapshot.successful_settlements,
            snapshot.successful_settlements_pct,
            snapshot.successful_settlements_pct_of_candidates,
            snapshot.settlements_precheck_failed,
            snapshot.settlements_precheck_failed_pct,
            snapshot.settlement_tx_attempts,
            snapshot.currently_open_status_orders,
            snapshot.currently_partially_filled_orders,
            snapshot.currently_filled_orders,
            snapshot.currently_cancelled_orders,
            snapshot.currently_stale_orders,
            snapshot.admission_balance_refreshes,
            snapshot.pre_settlement_balance_refreshes,
            snapshot.background_balance_refreshes,
            snapshot.cache_dirty_events,
        );
    }
}

fn pct_bar(pct: f64) -> String {
    let filled = ((pct.clamp(0.0, 100.0) / 100.0) * BAR_WIDTH as f64).round() as usize;
    format!(
        "[{}{}]",
        "#".repeat(filled),
        ".".repeat(BAR_WIDTH.saturating_sub(filled))
    )
}

fn pct(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 * 100.0 / denominator as f64
    }
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
