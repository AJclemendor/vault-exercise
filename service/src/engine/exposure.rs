use alloy::primitives::{Address, U256};
use std::cmp::Reverse;

use crate::types::{OrderStatus, OrderType};

use super::math::{reservation_for, sub_or_zero};
use super::{Engine, Order};

impl Engine {
    pub(super) fn hard_available_for_user(&self, user: Address) -> U256 {
        let real = self
            .balances
            .get(&user)
            .map(|balance| balance.real)
            .unwrap_or(U256::ZERO);
        sub_or_zero(real, self.hard_locked_for_user(user))
    }

    pub(super) fn user_has_in_flight_order(&self, user: Address) -> bool {
        self.orders
            .values()
            .any(|order| order.user == user && order.is_live() && order.in_flight_size > U256::ZERO)
    }

    pub(super) fn user_has_other_in_flight_order(&self, user: Address, order_id: &str) -> bool {
        self.orders.values().any(|order| {
            order.user == user
                && order.id != order_id
                && order.is_live()
                && order.in_flight_size > U256::ZERO
        })
    }

    pub(crate) fn stale_other_live_orders_for_user(&mut self, user: Address, keep_order_id: &str) {
        let mut candidates: Vec<_> = self
            .orders
            .values()
            .filter(|order| {
                order.user == user
                    && order.id != keep_order_id
                    && order.is_live()
                    && order.in_flight_size.is_zero()
            })
            .map(|order| (Reverse(order.created_seq), order.id.clone()))
            .collect();
        candidates.sort_by_key(|candidate| candidate.0);

        for (_, order_id) in candidates {
            self.terminal_order(&order_id, OrderStatus::Stale);
        }
    }

    pub(crate) fn stale_unsafe_live_orders_for_user(
        &mut self,
        user: Address,
        exclude_order_id: Option<&str>,
    ) {
        let mut candidates: Vec<_> = self
            .orders
            .values()
            .filter(|order| {
                order.user == user
                    && order.is_live()
                    && order.in_flight_size.is_zero()
                    && exclude_order_id
                        .map(|excluded| order.id != excluded)
                        .unwrap_or(true)
            })
            .map(|order| (Reverse(order.created_seq), order.id.clone()))
            .collect();
        candidates.sort_by_key(|candidate| candidate.0);

        for (_, order_id) in candidates {
            let Some(order) = self.orders.get(&order_id) else {
                continue;
            };
            if !order.is_live() || order.in_flight_size > U256::ZERO {
                continue;
            }

            let required = gross_required_for_order(order);
            let available = self.hard_available_for_user_excluding_order(user, &order_id);
            if required > available {
                self.terminal_order(&order_id, OrderStatus::Stale);
            }
        }
    }

    pub(super) fn required_balance_after_fill_for_order(
        &self,
        user: Address,
        order: &Order,
        fill_size: U256,
        settlement_debit: U256,
    ) -> Option<U256> {
        self.hard_locked_for_user_excluding_order(user, &order.id)
            .checked_add(hard_locked_after_fill(order, fill_size)?)?
            .checked_add(settlement_debit)
    }

    fn hard_locked_for_user(&self, user: Address) -> U256 {
        self.orders
            .values()
            .filter(|order| order.user == user && order.is_live())
            .fold(U256::ZERO, |total, order| {
                total + hard_locked_for_order(order)
            })
    }

    fn hard_locked_for_user_excluding_order(&self, user: Address, order_id: &str) -> U256 {
        self.orders
            .values()
            .filter(|order| order.user == user && order.id != order_id && order.is_live())
            .fold(U256::ZERO, |total, order| {
                total + hard_locked_for_order(order)
            })
    }

    fn hard_available_for_user_excluding_order(&self, user: Address, order_id: &str) -> U256 {
        let real = self
            .balances
            .get(&user)
            .map(|balance| balance.real)
            .unwrap_or(U256::ZERO);
        sub_or_zero(
            real,
            self.hard_locked_for_user_excluding_order(user, order_id),
        )
    }
}

fn gross_required_for_order(order: &Order) -> U256 {
    reservation_for(order.side, order.price, order.total_remaining())
        .expect("stored order reservation should be bounded")
}

fn hard_locked_for_order(order: &Order) -> U256 {
    match order.order_type {
        OrderType::Market => gross_required_for_order(order),
        OrderType::Limit if order.in_flight_size > U256::ZERO => {
            reservation_for(order.side, order.price, order.in_flight_size)
                .expect("stored order reservation should be bounded")
        }
        OrderType::Limit => U256::ZERO,
    }
}

fn hard_locked_after_fill(order: &Order, fill_size: U256) -> Option<U256> {
    if order.total_remaining() < fill_size || order.in_flight_size < fill_size {
        return None;
    }

    let post_fill_remaining = order.total_remaining() - fill_size;
    if order.order_type == OrderType::Market {
        return reservation_for(order.side, order.price, post_fill_remaining);
    }

    let post_fill_in_flight = order.in_flight_size - fill_size;
    reservation_for(order.side, order.price, post_fill_in_flight)
}
