use alloy::primitives::{Address, U256};

use crate::types::{
    ApiError, OrderResponse, OrderStatus, OrderType, OrderView, SubmitOrderRequest,
};

use super::math::{reservation_for, sub_or_zero};
use super::{Engine, Order, OrderAdmission};

impl Engine {
    pub(crate) fn submit_order(
        &mut self,
        request: SubmitOrderRequest,
    ) -> std::result::Result<OrderResponse, ApiError> {
        let admission = self.submit_order_and_match(request)?;
        self.pending_fills.extend(admission.fills);
        Ok(admission.response)
    }

    pub(crate) fn submit_order_and_claim_fills(
        &mut self,
        request: SubmitOrderRequest,
    ) -> std::result::Result<OrderAdmission, ApiError> {
        let previous_pending = self.pending_fills.len();
        let response = self.submit_order(request)?;
        let fills = self.pending_fills.drain(previous_pending..).collect();
        Ok(OrderAdmission { response, fills })
    }

    fn submit_order_and_match(
        &mut self,
        request: SubmitOrderRequest,
    ) -> std::result::Result<OrderAdmission, ApiError> {
        self.stats.orders_received += 1;

        if request.size.is_zero() {
            self.record_order_rejected_bad_request();
            return Err(ApiError::BadRequest(
                "order size must be greater than zero".into(),
            ));
        }
        if request.price.is_zero() {
            self.record_order_rejected_bad_request();
            return Err(ApiError::BadRequest(
                "order price must be greater than zero".into(),
            ));
        }

        self.prune_user_to_balance(request.user, None);

        let Some(required) = reservation_for(request.side, request.price, request.size) else {
            self.record_order_rejected_bad_request();
            return Err(ApiError::BadRequest(
                "order notional is too large to reserve safely".into(),
            ));
        };
        let stale_balance = {
            let balance = self.balances.entry(request.user).or_default();
            balance.dirty || balance.last_refresh.is_none()
        };
        if stale_balance {
            self.record_order_rejected_stale_balance_cache();
            return Err(ApiError::Chain(
                "balance cache is not fresh enough for admission".into(),
            ));
        }

        let available = self.hard_available_for_user(request.user);
        if available < required {
            self.record_order_rejected_insufficient_balance();
            return Err(ApiError::BadRequest(format!(
                "insufficient available balance: required={required}, available={available}"
            )));
        }

        self.balances.entry(request.user).or_default().reserved += required;

        let id = format!("ord-{}", self.next_order_seq);
        let order = Order {
            id: id.clone(),
            user: request.user,
            side: request.side,
            order_type: request.order_type,
            price: request.price,
            size: request.size,
            filled_size: U256::ZERO,
            in_flight_size: U256::ZERO,
            status: OrderStatus::Open,
            created_seq: self.next_order_seq,
            cancel_requested: false,
            matched_once: false,
        };
        self.next_order_seq += 1;
        self.orders.insert(id.clone(), order);

        let fills = self.match_new_order(&id);
        let order_type = self
            .orders
            .get(&id)
            .map(|order| order.order_type)
            .expect("submitted order should exist");
        if order_type == OrderType::Limit {
            let (side, price) = self
                .orders
                .get(&id)
                .map(|order| (order.side, order.price))
                .expect("submitted order should exist");
            self.index_limit_order(side, price, id.clone());
        }
        if request.order_type == OrderType::Market {
            self.cancel_unfilled_market_remainder(&id);
            self.prune_user_to_balance(request.user, Some(id.clone()));
        }
        self.stats.orders_accepted += 1;
        let status = self
            .orders
            .get(&id)
            .map(|order| order.status)
            .unwrap_or(OrderStatus::Cancelled);

        Ok(OrderAdmission {
            response: OrderResponse {
                order_id: id,
                status,
            },
            fills,
        })
    }

    pub(crate) fn cancel_order(&mut self, order_id: &str) -> std::result::Result<(), ApiError> {
        let Some(order) = self.orders.get(order_id) else {
            return Err(ApiError::NotFound(format!("order {order_id} not found")));
        };

        if !order.is_live() {
            return Ok(());
        }

        if order.in_flight_size > U256::ZERO {
            if let Some(order) = self.orders.get_mut(order_id) {
                order.cancel_requested = true;
            }
            return Ok(());
        }

        self.terminal_order(order_id, OrderStatus::Cancelled);
        Ok(())
    }

    pub(crate) fn open_orders(&self, user: Option<Address>) -> Vec<OrderView> {
        let mut orders: Vec<_> = self
            .orders
            .values()
            .filter(|order| order.is_live())
            .filter(|order| user.map(|u| order.user == u).unwrap_or(true))
            .collect();
        orders.sort_by_key(|order| order.created_seq);
        orders.into_iter().map(Order::view).collect()
    }

    pub(super) fn terminal_order(&mut self, order_id: &str, status: OrderStatus) {
        let Some(order_snapshot) = self.orders.get(order_id).cloned() else {
            return;
        };
        if !order_snapshot.is_live() {
            return;
        }

        let release = reservation_for(
            order_snapshot.side,
            order_snapshot.price,
            order_snapshot.total_remaining(),
        )
        .expect("stored order reservation should be bounded");

        if let Some(order) = self.orders.get_mut(order_id) {
            order.in_flight_size = U256::ZERO;
            order.status = status;
            order.cancel_requested = false;
        }

        self.release_user_reservation(order_snapshot.user, release);

        if status == OrderStatus::Stale {
            self.stats.orders_marked_stale += 1;
        }
    }

    pub(super) fn release_user_reservation(&mut self, user: Address, amount: U256) {
        let balance = self.balances.entry(user).or_default();
        balance.reserved = sub_or_zero(balance.reserved, amount);
    }

    fn cancel_unfilled_market_remainder(&mut self, order_id: &str) {
        let Some(order_snapshot) = self.orders.get(order_id).cloned() else {
            return;
        };
        if order_snapshot.order_type != OrderType::Market || !order_snapshot.is_live() {
            return;
        }

        if order_snapshot.in_flight_size.is_zero() {
            self.terminal_order(order_id, OrderStatus::Cancelled);
            return;
        }

        let retained_size = order_snapshot.filled_size + order_snapshot.in_flight_size;
        let release_size = sub_or_zero(order_snapshot.size, retained_size);
        let release = reservation_for(order_snapshot.side, order_snapshot.price, release_size)
            .expect("stored order reservation should be bounded");

        if let Some(order) = self.orders.get_mut(order_id) {
            order.size = retained_size;
            order.cancel_requested = true;
        }
        self.release_user_reservation(order_snapshot.user, release);
    }

    fn record_order_rejected_bad_request(&mut self) {
        self.stats.orders_rejected += 1;
        self.stats.orders_rejected_bad_request += 1;
    }

    fn record_order_rejected_insufficient_balance(&mut self) {
        self.stats.orders_rejected += 1;
        self.stats.orders_rejected_insufficient_balance += 1;
    }

    fn record_order_rejected_stale_balance_cache(&mut self) {
        self.stats.orders_rejected += 1;
        self.stats.orders_rejected_stale_balance_cache += 1;
    }
}
