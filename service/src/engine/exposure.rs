use alloy::primitives::{Address, U256};

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
        self.in_flight_orders_by_user
            .get(&user)
            .copied()
            .unwrap_or(0)
            > 0
    }

    pub(super) fn user_has_other_in_flight_order(&self, user: Address, order_id: &str) -> bool {
        match self
            .in_flight_orders_by_user
            .get(&user)
            .copied()
            .unwrap_or(0)
        {
            0 => false,
            1 => self
                .orders
                .get(order_id)
                .map(|order| order.in_flight_size.is_zero())
                .unwrap_or(true),
            _ => true,
        }
    }

    pub(crate) fn stale_over_reserved_orders_for_user(
        &mut self,
        user: Address,
        exclude_order_id: Option<&str>,
    ) {
        let Some(balance) = self.balances.get(&user) else {
            return;
        };
        if balance.reserved <= balance.real {
            return;
        }

        let candidates: Vec<_> = self
            .live_order_ids_for_user(user)
            .into_iter()
            .filter(|order_id| {
                exclude_order_id
                    .map(|excluded| order_id != excluded)
                    .unwrap_or(true)
            })
            .filter(|order_id| {
                self.orders
                    .get(order_id)
                    .map(|order| order.in_flight_size.is_zero())
                    .unwrap_or(false)
            })
            .collect();

        for order_id in candidates {
            self.terminal_order(&order_id, OrderStatus::Stale);
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
        self.live_order_ids_for_user(user)
            .iter()
            .filter_map(|order_id| self.orders.get(order_id))
            .fold(U256::ZERO, |total, order| {
                total + hard_locked_for_order(order)
            })
    }

    fn hard_locked_for_user_excluding_order(&self, user: Address, order_id: &str) -> U256 {
        self.live_order_ids_for_user(user)
            .iter()
            .filter(|id| id.as_str() != order_id)
            .filter_map(|id| self.orders.get(id))
            .fold(U256::ZERO, |total, order| {
                total + hard_locked_for_order(order)
            })
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
