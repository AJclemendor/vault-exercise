use alloy::primitives::{Address, U256};

use crate::types::{OrderStatus, OrderType, Side};

use super::math::{min_u256, reservation_for, sub_or_zero};
use super::{Engine, FillCandidate, Order};

impl Engine {
    pub(crate) fn claim_next_fill_candidate(&mut self) -> Option<FillCandidate> {
        if let Some(fill) = self.pop_pending_fill() {
            return Some(fill);
        }
        self.next_fill_candidate()
    }

    pub(crate) fn claim_fill_batch(&mut self, max: usize) -> Vec<FillCandidate> {
        let mut fills = Vec::new();
        while fills.len() < max {
            let Some(fill) = self.claim_next_fill_candidate() else {
                break;
            };
            fills.push(fill);
        }
        fills
    }

    pub(crate) fn next_fill_candidate(&mut self) -> Option<FillCandidate> {
        if let Some(fill) = self.pop_pending_fill() {
            return Some(fill);
        }

        loop {
            if let Some(market_id) = self.oldest_market_order() {
                if let Some(counterparty_id) = self.find_market_counterparty(&market_id) {
                    return self.prepare_fill(&market_id, &counterparty_id);
                }

                // Market orders are background IOC: they never appear in book
                // depth and are cancelled when no older resting limit can fill.
                self.terminal_order(&market_id, OrderStatus::Cancelled);
                continue;
            }

            let (buy_id, sell_id) = self.best_crossing_limits()?;
            return self.prepare_fill(&buy_id, &sell_id);
        }
    }

    pub(super) fn match_new_order(&mut self, order_id: &str) -> Vec<FillCandidate> {
        let mut fills = Vec::new();

        while self.order_can_take(order_id) {
            let Some(counterparty_id) = self.find_taker_counterparty(order_id) else {
                break;
            };
            let Some(fill) = self.prepare_taker_fill(order_id, &counterparty_id) else {
                break;
            };
            fills.push(fill);
        }

        fills
    }

    fn order_can_take(&self, order_id: &str) -> bool {
        self.orders
            .get(order_id)
            .map(|order| {
                order.is_live()
                    && order.available_remaining() > U256::ZERO
                    && !self.user_has_other_in_flight_order(order.user, order_id)
            })
            .unwrap_or(false)
    }

    fn find_taker_counterparty(&mut self, order_id: &str) -> Option<String> {
        let taker = self.orders.get(order_id)?.clone();
        let (counterparty_side, prices): (Side, Vec<_>) = match taker.side {
            Side::Buy => (
                Side::Sell,
                self.asks
                    .range(..=taker.price)
                    .map(|(price, _)| *price)
                    .collect(),
            ),
            Side::Sell => (
                Side::Buy,
                self.bids
                    .range(taker.price..)
                    .rev()
                    .map(|(price, _)| *price)
                    .collect(),
            ),
        };

        for price in prices {
            for candidate_id in self.available_limit_ids_at_price(counterparty_side, price) {
                let Some(candidate) = self.orders.get(&candidate_id) else {
                    continue;
                };
                if candidate.user != taker.user {
                    return Some(candidate_id);
                }
            }
        }

        None
    }

    fn oldest_market_order(&self) -> Option<String> {
        self.orders
            .values()
            .filter(|order| order.is_available_for_fill() && order.order_type == OrderType::Market)
            .filter(|order| !self.user_has_in_flight_order(order.user))
            .min_by_key(|order| order.created_seq)
            .map(|order| order.id.clone())
    }

    fn find_market_counterparty(&mut self, market_id: &str) -> Option<String> {
        let market = self.orders.get(market_id)?.clone();

        let prices: Vec<_> = match market.side {
            Side::Buy => self
                .asks
                .range(..=market.price)
                .map(|(price, _)| *price)
                .collect(),
            Side::Sell => self
                .bids
                .range(market.price..)
                .rev()
                .map(|(price, _)| *price)
                .collect(),
        };
        let counterparty_side = match market.side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };

        for price in prices {
            for order_id in self.available_limit_ids_at_price(counterparty_side, price) {
                let Some(order) = self.orders.get(&order_id) else {
                    continue;
                };
                if order.user != market.user && order.created_seq < market.created_seq {
                    return Some(order_id);
                }
            }
        }

        None
    }

    pub(super) fn best_crossing_limits(&mut self) -> Option<(String, String)> {
        let bid_prices: Vec<_> = self.bids.keys().rev().copied().collect();

        for bid_price in bid_prices {
            let buy_ids = self.available_limit_ids_at_price(Side::Buy, bid_price);
            if buy_ids.is_empty() {
                continue;
            }

            let ask_prices: Vec<_> = self
                .asks
                .range(..=bid_price)
                .map(|(price, _)| *price)
                .collect();
            for ask_price in ask_prices {
                let sell_ids = self.available_limit_ids_at_price(Side::Sell, ask_price);
                if sell_ids.is_empty() {
                    continue;
                }

                for buy_id in &buy_ids {
                    let Some(buy_user) = self.orders.get(buy_id).map(|order| order.user) else {
                        continue;
                    };
                    if let Some(sell_id) = sell_ids.iter().find(|sell_id| {
                        self.orders
                            .get(*sell_id)
                            .map(|sell| sell.user != buy_user)
                            .unwrap_or(false)
                    }) {
                        return Some((buy_id.clone(), sell_id.clone()));
                    }
                }
            }
        }

        None
    }

    fn prepare_fill(&mut self, first_id: &str, second_id: &str) -> Option<FillCandidate> {
        self.prepare_fill_with_taker(first_id, second_id, None)
    }

    fn prepare_taker_fill(&mut self, taker_id: &str, resting_id: &str) -> Option<FillCandidate> {
        self.prepare_fill_with_taker(taker_id, resting_id, Some(taker_id))
    }

    fn prepare_fill_with_taker(
        &mut self,
        first_id: &str,
        second_id: &str,
        taker_id: Option<&str>,
    ) -> Option<FillCandidate> {
        let first = self.orders.get(first_id)?;
        let second = self.orders.get(second_id)?;
        let (buy_id, sell_id) = match (first.side, second.side) {
            (Side::Buy, Side::Sell) => (first.id.clone(), second.id.clone()),
            (Side::Sell, Side::Buy) => (second.id.clone(), first.id.clone()),
            _ => return None,
        };

        let buy = self.orders.get(&buy_id)?;
        let sell = self.orders.get(&sell_id)?;
        if buy.user == sell.user {
            return None;
        }
        if !self.order_is_fillable(&buy_id, taker_id) || !self.order_is_fillable(&sell_id, taker_id)
        {
            return None;
        }

        let fill_size = min_u256(buy.available_remaining(), sell.available_remaining());
        if fill_size.is_zero() {
            return None;
        }

        let exec_price = execution_price(buy, sell);
        let quote = reservation_for(Side::Buy, exec_price, fill_size)?;
        if quote.is_zero() {
            return None;
        }

        let fill_seq = self.next_fill_seq;
        self.next_fill_seq += 1;

        let candidate = FillCandidate {
            seq: fill_seq,
            buy_id: buy_id.clone(),
            sell_id: sell_id.clone(),
            buyer: buy.user,
            seller: sell.user,
            fill_size,
            exec_price,
            quote,
            base: fill_size,
        };

        self.add_order_in_flight(&buy_id, fill_size);
        self.add_order_in_flight(&sell_id, fill_size);
        self.track_in_flight_fill(&candidate);

        self.stats.fill_candidates += 1;
        Some(candidate)
    }

    fn order_is_fillable(&self, order_id: &str, taker_id: Option<&str>) -> bool {
        let Some(order) = self.orders.get(order_id) else {
            return false;
        };

        if taker_id == Some(order_id) {
            return order.is_live()
                && order.available_remaining() > U256::ZERO
                && !self.user_has_other_in_flight_order(order.user, order_id);
        }

        order.is_available_for_fill() && !self.user_has_in_flight_order(order.user)
    }

    pub(crate) fn fill_still_pending(&self, fill: &FillCandidate) -> bool {
        let Some(tracked) = self.in_flight_fills.get(&fill.seq) else {
            return false;
        };
        if tracked.buy_id != fill.buy_id
            || tracked.sell_id != fill.sell_id
            || tracked.fill_size != fill.fill_size
        {
            return false;
        }
        let Some(buy) = self.orders.get(&fill.buy_id) else {
            return false;
        };
        let Some(sell) = self.orders.get(&fill.sell_id) else {
            return false;
        };
        buy.is_live()
            && sell.is_live()
            && buy.in_flight_size >= fill.fill_size
            && sell.in_flight_size >= fill.fill_size
    }

    pub(crate) fn users_funded_for_reserved(&self, fill: &FillCandidate) -> (bool, bool) {
        let buyer_ok = self.user_funded_for_fill(fill, fill.buyer);
        let seller_ok = self.user_funded_for_fill(fill, fill.seller);
        (buyer_ok, seller_ok)
    }

    pub(crate) fn prune_underfunded_fill_users(&mut self, fill: &FillCandidate) -> (bool, bool) {
        let (buyer_ok, seller_ok) = self.users_funded_for_reserved(fill);
        if !buyer_ok {
            self.prune_user_to_afford_fill(fill.buyer, fill);
        }
        if !seller_ok {
            self.prune_user_to_afford_fill(fill.seller, fill);
        }
        self.users_funded_for_reserved(fill)
    }

    fn user_funded_for_fill(&self, fill: &FillCandidate, user: Address) -> bool {
        let Some(balance) = self.balances.get(&user) else {
            return false;
        };
        if balance.dirty || balance.last_refresh.is_none() {
            return false;
        }
        self.required_balance_after_fill_for_user(fill, user)
            .map(|required| balance.real >= required)
            .unwrap_or(false)
    }

    pub(crate) fn fill_balances_are_fresh(&self, fill: &FillCandidate) -> bool {
        self.balance_cache_is_fresh(fill.buyer)
            && (fill.seller == fill.buyer || self.balance_cache_is_fresh(fill.seller))
    }

    fn required_balance_after_fill_for_user(
        &self,
        fill: &FillCandidate,
        user: Address,
    ) -> Option<U256> {
        let mut required = U256::ZERO;

        if fill.buyer == user {
            required = self
                .required_balance_after_order_fill(&fill.buy_id, fill.fill_size, fill.quote)?
                .max(required);
        }
        if fill.seller == user {
            required = self
                .required_balance_after_order_fill(&fill.sell_id, fill.fill_size, fill.base)?
                .max(required);
        }

        Some(required)
    }

    fn required_balance_after_order_fill(
        &self,
        order_id: &str,
        fill_size: U256,
        settlement_debit: U256,
    ) -> Option<U256> {
        let order = self.orders.get(order_id)?;
        self.required_balance_after_fill_for_order(order.user, order, fill_size, settlement_debit)
    }

    pub(crate) fn apply_settlement_success(&mut self, fill: &FillCandidate) {
        self.apply_settlement_success_inner(fill, true);
    }

    pub(crate) fn apply_settlement_success_without_balance_prune(&mut self, fill: &FillCandidate) {
        self.apply_settlement_success_inner(fill, false);
    }

    fn apply_settlement_success_inner(&mut self, fill: &FillCandidate, prune_balances: bool) {
        if !self.fill_still_pending(fill) {
            return;
        }
        self.untrack_in_flight_fill(fill.seq);

        let matched_orders = self.apply_order_fill(&fill.buy_id, fill.fill_size) as u64
            + self.apply_order_fill(&fill.sell_id, fill.fill_size) as u64;

        if prune_balances {
            self.stale_over_reserved_orders_for_user(fill.buyer, None);
            if fill.seller != fill.buyer {
                self.stale_over_reserved_orders_for_user(fill.seller, None);
            }
        }

        self.stats.unique_orders_with_successful_fill += matched_orders;
        self.stats.order_sides_filled += 2;
        self.stats.successful_settlements += 1;
    }

    fn apply_order_fill(&mut self, order_id: &str, fill_size: U256) -> bool {
        let Some(order_snapshot) = self.orders.get(order_id).cloned() else {
            return false;
        };
        let old_required = reservation_for(
            order_snapshot.side,
            order_snapshot.price,
            order_snapshot.total_remaining(),
        )
        .expect("stored order reservation should be bounded");

        let mut cancel_after_fill = false;
        let mut terminal_filled = false;
        let release_after_fill: U256;
        let user;
        let first_successful_fill;
        let in_flight_transition;

        {
            let Some(order) = self.orders.get_mut(order_id) else {
                return false;
            };
            user = order.user;
            first_successful_fill = !order.matched_once;
            order.matched_once = true;
            let was_in_flight = order.is_live() && order.in_flight_size > U256::ZERO;
            order.in_flight_size = sub_or_zero(order.in_flight_size, fill_size);
            order.filled_size += fill_size;

            if order.filled_size >= order.size {
                order.filled_size = order.size;
                order.status = OrderStatus::Filled;
                order.cancel_requested = false;
                terminal_filled = true;
            } else {
                order.status = OrderStatus::PartiallyFilled;
                cancel_after_fill = order.cancel_requested && order.in_flight_size.is_zero();
            }
            let is_in_flight = order.is_live() && order.in_flight_size > U256::ZERO;
            in_flight_transition = (user, was_in_flight, is_in_flight);

            let new_required = if order.is_live() {
                reservation_for(order.side, order.price, order.total_remaining())
                    .expect("stored order reservation should be bounded")
            } else {
                U256::ZERO
            };
            release_after_fill = sub_or_zero(old_required, new_required);
        }

        self.apply_in_flight_transition(in_flight_transition);

        if release_after_fill > U256::ZERO {
            self.release_user_reservation(user, release_after_fill);
        }

        if terminal_filled {
            self.untrack_live_order(user, order_id);
        }

        if cancel_after_fill {
            self.terminal_order(order_id, OrderStatus::Cancelled);
        }

        first_successful_fill
    }

    pub(crate) fn abort_fill(&mut self, fill: &FillCandidate, stale_buy: bool, stale_sell: bool) {
        if !self.fill_still_pending(fill) {
            return;
        }
        self.untrack_in_flight_fill(fill.seq);

        if stale_buy {
            self.terminal_order(&fill.buy_id, OrderStatus::Stale);
        } else {
            self.release_inflight(&fill.buy_id, fill.fill_size);
        }

        if stale_sell {
            self.terminal_order(&fill.sell_id, OrderStatus::Stale);
        } else {
            self.release_inflight(&fill.sell_id, fill.fill_size);
        }
    }

    fn prune_user_to_afford_fill(&mut self, user: Address, fill: &FillCandidate) {
        if self.user_funded_for_fill(fill, user) {
            return;
        }

        self.stale_over_reserved_orders_for_user(user, None);
    }
}

fn execution_price(buy: &Order, sell: &Order) -> U256 {
    match (buy.order_type, sell.order_type) {
        (OrderType::Market, OrderType::Limit) => sell.price,
        (OrderType::Limit, OrderType::Market) => buy.price,
        _ if buy.created_seq > sell.created_seq => sell.price,
        _ => buy.price,
    }
}
