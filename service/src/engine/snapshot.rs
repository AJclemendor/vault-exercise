use alloy::primitives::U256;

use crate::stats::{StatsSnapshot, pct};
use crate::types::{OrderStatus, OrderType};

use super::Engine;

impl Engine {
    pub(crate) fn stats_snapshot(&self) -> StatsSnapshot {
        let live_orders = self.orders.values().filter(|order| order.is_live()).count();
        let open_market_ioc_orders = self
            .orders
            .values()
            .filter(|order| order.is_live() && order.order_type == OrderType::Market)
            .count();
        let market_ioc_orders_accepted = self
            .orders
            .values()
            .filter(|order| order.order_type == OrderType::Market)
            .count() as u64;
        let market_ioc_orders_cancelled_unfilled = self
            .orders
            .values()
            .filter(|order| {
                order.order_type == OrderType::Market
                    && order.status == OrderStatus::Cancelled
                    && order.filled_size.is_zero()
            })
            .count();
        let open_status_orders = self
            .orders
            .values()
            .filter(|order| order.status == OrderStatus::Open)
            .count();
        let partially_filled_orders = self
            .orders
            .values()
            .filter(|order| order.status == OrderStatus::PartiallyFilled)
            .count();
        let filled_orders = self
            .orders
            .values()
            .filter(|order| order.status == OrderStatus::Filled)
            .count();
        let cancelled_orders = self
            .orders
            .values()
            .filter(|order| order.status == OrderStatus::Cancelled)
            .count();
        let stale_orders = self
            .orders
            .values()
            .filter(|order| order.status == OrderStatus::Stale)
            .count();

        let active_cache_ages: Vec<u128> = self
            .balances
            .values()
            .filter(|balance| balance.reserved > U256::ZERO)
            .filter_map(|balance| balance.last_refresh.map(|last| last.elapsed().as_millis()))
            .collect();
        let average_active_cache_age_ms = if active_cache_ages.is_empty() {
            0
        } else {
            (active_cache_ages.iter().sum::<u128>() / active_cache_ages.len() as u128) as u64
        };
        let orders_admission_failures = self.stats.orders_rejected_bad_request
            + self.stats.orders_rejected_insufficient_balance
            + self.stats.orders_rejected_stale_balance_cache
            + self.stats.orders_failed_balance_refresh;
        let settlement_tx_failures = self.stats.settlement_send_failures
            + self.stats.settlement_receipt_failures
            + self.stats.settlements_reverted;
        let settlement_failures = self.stats.settlements_precheck_failed + settlement_tx_failures;

        StatsSnapshot {
            orders_received: self.stats.orders_received,
            orders_accepted: self.stats.orders_accepted,
            orders_accepted_pct: pct(self.stats.orders_accepted, self.stats.orders_received),
            orders_rejected: self.stats.orders_rejected,
            orders_rejected_pct: pct(self.stats.orders_rejected, self.stats.orders_received),
            orders_admission_failures,
            orders_admission_failures_pct: pct(
                orders_admission_failures,
                self.stats.orders_received,
            ),
            orders_rejected_bad_request: self.stats.orders_rejected_bad_request,
            orders_rejected_bad_request_pct: pct(
                self.stats.orders_rejected_bad_request,
                self.stats.orders_received,
            ),
            orders_rejected_insufficient_balance: self.stats.orders_rejected_insufficient_balance,
            orders_rejected_insufficient_balance_pct: pct(
                self.stats.orders_rejected_insufficient_balance,
                self.stats.orders_received,
            ),
            orders_rejected_stale_balance_cache: self.stats.orders_rejected_stale_balance_cache,
            orders_rejected_stale_balance_cache_pct: pct(
                self.stats.orders_rejected_stale_balance_cache,
                self.stats.orders_received,
            ),
            orders_failed_balance_refresh: self.stats.orders_failed_balance_refresh,
            orders_failed_balance_refresh_pct: pct(
                self.stats.orders_failed_balance_refresh,
                self.stats.orders_received,
            ),
            orders_matched: self.stats.order_sides_filled,
            orders_with_successful_fill: self.stats.unique_orders_with_successful_fill,
            unique_orders_with_successful_fill: self.stats.unique_orders_with_successful_fill,
            order_sides_filled: self.stats.order_sides_filled,
            fill_sides_successfully_settled: self.stats.order_sides_filled,
            market_ioc_orders_accepted,
            market_ioc_orders_cancelled_unfilled,
            currently_open_market_ioc_orders: open_market_ioc_orders,
            fill_candidates: self.stats.fill_candidates,
            orders_matched_pct_of_accepted: pct(
                self.stats.order_sides_filled,
                self.stats.orders_accepted,
            ),
            unique_orders_with_successful_fill_pct_of_accepted: pct(
                self.stats.unique_orders_with_successful_fill,
                self.stats.orders_accepted,
            ),
            settlements_attempted: self.stats.settlements_attempted,
            settlement_precheck_attempts: self.stats.settlements_attempted,
            settlement_tx_attempts: self.stats.settlement_tx_attempts,
            settlement_failures,
            settlement_failures_pct: pct(settlement_failures, self.stats.settlements_attempted),
            settlement_tx_failures,
            settlement_tx_failures_pct: pct(
                settlement_tx_failures,
                self.stats.settlement_tx_attempts,
            ),
            settlements_precheck_failed: self.stats.settlements_precheck_failed,
            settlement_precheck_failures: self.stats.settlements_precheck_failed,
            settlements_precheck_failed_pct: pct(
                self.stats.settlements_precheck_failed,
                self.stats.settlements_attempted,
            ),
            settlement_send_failures: self.stats.settlement_send_failures,
            settlement_send_failures_pct: pct(
                self.stats.settlement_send_failures,
                self.stats.settlement_tx_attempts,
            ),
            settlement_receipt_failures: self.stats.settlement_receipt_failures,
            settlement_receipt_failures_pct: pct(
                self.stats.settlement_receipt_failures,
                self.stats.settlement_tx_attempts,
            ),
            settlements_reverted: self.stats.settlements_reverted,
            settlement_reverts: self.stats.settlements_reverted,
            settlement_receipt_status_reverted: self.stats.settlements_reverted,
            settlement_tx_reverts: self.stats.settlements_reverted,
            settlement_unknown_outcomes: self.stats.settlement_unknown_outcomes,
            settlements_reverted_pct: pct(
                self.stats.settlements_reverted,
                self.stats.settlement_tx_attempts,
            ),
            currently_open_orders: live_orders,
            currently_open_orders_pct_of_accepted: pct(
                live_orders as u64,
                self.stats.orders_accepted,
            ),
            currently_live_orders: live_orders,
            currently_live_orders_pct_of_accepted: pct(
                live_orders as u64,
                self.stats.orders_accepted,
            ),
            currently_open_status_orders: open_status_orders,
            currently_partially_filled_orders: partially_filled_orders,
            currently_filled_orders: filled_orders,
            currently_filled_orders_pct_of_accepted: pct(
                filled_orders as u64,
                self.stats.orders_accepted,
            ),
            currently_cancelled_orders: cancelled_orders,
            currently_stale_orders: stale_orders,
            currently_stale_orders_pct_of_accepted: pct(
                stale_orders as u64,
                self.stats.orders_accepted,
            ),
            successful_settlements: self.stats.successful_settlements,
            fills_settled: self.stats.successful_settlements,
            fills_successfully_settled: self.stats.successful_settlements,
            successful_settlements_pct: pct(
                self.stats.successful_settlements,
                self.stats.settlements_attempted,
            ),
            successful_settlements_pct_of_candidates: pct(
                self.stats.successful_settlements,
                self.stats.fill_candidates,
            ),
            successful_settlements_pct_of_accepted: pct(
                self.stats.successful_settlements,
                self.stats.orders_accepted,
            ),
            orders_marked_stale: self.stats.orders_marked_stale,
            orders_marked_stale_pct_of_accepted: pct(
                self.stats.orders_marked_stale,
                self.stats.orders_accepted,
            ),
            admission_balance_refreshes: self.stats.admission_balance_refreshes,
            pre_settlement_balance_refreshes: self.stats.pre_settlement_balance_refreshes,
            background_balance_refreshes: self.stats.background_balance_refreshes,
            cache_dirty_events: self.stats.cache_dirty_events,
            average_active_cache_age_ms,
        }
    }

    pub(crate) fn record_admission_refresh_succeeded(&mut self) {
        self.stats.admission_balance_refreshes += 1;
    }

    pub(crate) fn record_admission_refresh_failed(&mut self) {
        self.stats.orders_received += 1;
        self.stats.orders_rejected += 1;
        self.stats.orders_failed_balance_refresh += 1;
    }

    pub(crate) fn record_settlement_attempted(&mut self) {
        self.stats.settlements_attempted += 1;
    }

    pub(crate) fn record_settlement_precheck_failed(&mut self) {
        self.stats.settlements_precheck_failed += 1;
    }

    pub(crate) fn record_settlement_tx_attempt(&mut self) {
        self.stats.settlement_tx_attempts += 1;
    }

    pub(crate) fn record_settlement_reverted(&mut self) {
        self.stats.settlements_reverted += 1;
    }

    pub(crate) fn record_settlement_send_failed(&mut self) {
        self.stats.settlement_send_failures += 1;
    }

    pub(crate) fn record_settlement_receipt_failed(&mut self) {
        self.stats.settlement_receipt_failures += 1;
    }

    pub(crate) fn record_settlement_unknown_outcome(&mut self) {
        self.stats.settlement_unknown_outcomes += 1;
    }

    pub(crate) fn record_pre_settlement_balance_refreshes(&mut self, count: u64) {
        self.stats.pre_settlement_balance_refreshes += count;
    }

    pub(crate) fn record_background_balance_refresh(&mut self) {
        self.stats.background_balance_refreshes += 1;
    }
}
