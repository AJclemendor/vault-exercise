use alloy::primitives::{Address, U256};

use crate::types::{
    ApiError, OrderResponse, OrderStatus, OrderType, OrderView, SubmitOrderRequest,
};

use super::math::{reservation_for, sub_or_zero};
use super::{Engine, FillCandidate, Order, OrderAdmission};

impl Engine {
    pub(crate) fn validate_order_request(
        request: &SubmitOrderRequest,
    ) -> std::result::Result<(), ApiError> {
        if request.size.is_zero() {
            return Err(ApiError::BadRequest(
                "order size must be greater than zero".into(),
            ));
        }
        if request.price.is_zero() {
            return Err(ApiError::BadRequest(
                "order price must be greater than zero".into(),
            ));
        }
        if reservation_for(request.side, request.price, request.size).is_none() {
            return Err(ApiError::BadRequest(
                "order notional is too large to reserve safely".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn record_order_shape_rejection(&mut self) {
        self.stats.orders_received += 1;
        self.record_order_rejected_bad_request();
    }

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

        let required = match Self::validate_order_request(&request) {
            Ok(()) => reservation_for(request.side, request.price, request.size)
                .expect("validated order reservation should be bounded"),
            Err(err) => {
                self.record_order_rejected_bad_request();
                return Err(err);
            }
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

        self.prune_user_to_balance(request.user, None);

        let available = self.hard_available_for_user(request.user);
        if available < required {
            self.record_order_rejected_insufficient_balance();
            return Err(ApiError::BadRequest(format!(
                "insufficient available balance: required={required}, available={available}"
            )));
        }

        let Some(new_reserved) = self
            .balances
            .entry(request.user)
            .or_default()
            .reserved
            .checked_add(required)
        else {
            self.record_order_rejected_bad_request();
            return Err(ApiError::BadRequest(
                "reserved balance accounting would overflow".into(),
            ));
        };
        self.balances.entry(request.user).or_default().reserved = new_reserved;

        let id = format!("ord-{}", self.next_order_seq);
        let order = Order {
            id: id.clone(),
            user: request.user,
            side: request.side,
            order_type: request.order_type,
            price: request.price,
            size: request.size,
            reserved: required,
            filled_size: U256::ZERO,
            in_flight_size: U256::ZERO,
            status: OrderStatus::Open,
            created_seq: self.next_order_seq,
            cancel_requested: false,
            matched_once: false,
        };
        self.next_order_seq += 1;
        self.orders.insert(id.clone(), order);
        self.track_live_order(request.user, id.clone());

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

    pub(crate) fn abort_admission_after_queue_failure(
        &mut self,
        order_id: &str,
        fills: &[FillCandidate],
        first_unsent: usize,
    ) {
        for fill in fills.iter().skip(first_unsent) {
            if self.fill_still_pending(fill) {
                self.record_settlement_aborted_before_tx();
                self.abort_fill(fill, false, false);
            }
        }
        let _ = self.cancel_order(order_id);
    }

    pub(crate) fn open_orders(&self, user: Option<Address>) -> Vec<OrderView> {
        let mut orders: Vec<_> = self
            .orders
            .values()
            .filter(|order| order.is_visible_open())
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

        self.release_related_in_flight_fills(order_id);

        let release = order_snapshot.reserved;

        if let Some(order) = self.orders.get_mut(order_id) {
            order.reserved = U256::ZERO;
            order.in_flight_size = U256::ZERO;
            order.status = status;
            order.cancel_requested = false;
        }
        self.clear_order_in_flight(order_snapshot.user, order_snapshot.in_flight_size);
        self.untrack_live_order(order_snapshot.user, order_id);

        self.release_user_reservation(order_snapshot.user, release);

        if status == OrderStatus::Stale {
            self.stats.orders_marked_stale += 1;
        }
    }

    pub(super) fn release_user_reservation(&mut self, user: Address, amount: U256) {
        let balance = self.balances.entry(user).or_default();
        balance.reserved = sub_or_zero(balance.reserved, amount);
    }

    pub(super) fn track_live_order(&mut self, user: Address, order_id: String) {
        self.live_order_ids_by_user
            .entry(user)
            .or_default()
            .insert(order_id);
    }

    pub(super) fn untrack_live_order(&mut self, user: Address, order_id: &str) {
        if let Some(order_ids) = self.live_order_ids_by_user.get_mut(&user) {
            order_ids.remove(order_id);
            if order_ids.is_empty() {
                self.live_order_ids_by_user.remove(&user);
            }
        }
    }

    pub(super) fn live_order_ids_for_user(&self, user: Address) -> Vec<String> {
        let mut order_ids: Vec<_> = self
            .live_order_ids_by_user
            .get(&user)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_else(|| {
                self.orders
                    .values()
                    .filter(|order| order.user == user && order.is_live())
                    .map(|order| order.id.clone())
                    .collect()
            });
        order_ids.sort_by_key(|order_id| {
            self.orders
                .get(order_id)
                .map(|order| order.created_seq)
                .unwrap_or(u64::MAX)
        });
        order_ids
    }

    pub(super) fn add_order_in_flight(&mut self, order_id: &str, amount: U256) {
        let transition = {
            let Some(order) = self.orders.get_mut(order_id) else {
                return;
            };
            let was_in_flight = order.is_live() && order.in_flight_size > U256::ZERO;
            order.in_flight_size += amount;
            let is_in_flight = order.is_live() && order.in_flight_size > U256::ZERO;
            (order.user, was_in_flight, is_in_flight)
        };
        self.apply_in_flight_transition(transition);
    }

    pub(super) fn subtract_order_in_flight(&mut self, order_id: &str, amount: U256) -> bool {
        let transition_and_cancel = {
            let Some(order) = self.orders.get_mut(order_id) else {
                return false;
            };
            let was_in_flight = order.is_live() && order.in_flight_size > U256::ZERO;
            order.in_flight_size = sub_or_zero(order.in_flight_size, amount);
            let is_in_flight = order.is_live() && order.in_flight_size > U256::ZERO;
            let cancel_after_release = order.cancel_requested && order.in_flight_size.is_zero();
            (
                order.user,
                was_in_flight,
                is_in_flight,
                cancel_after_release,
            )
        };
        let (user, was_in_flight, is_in_flight, cancel_after_release) = transition_and_cancel;
        self.apply_in_flight_transition((user, was_in_flight, is_in_flight));
        cancel_after_release
    }

    pub(super) fn clear_order_in_flight(&mut self, user: Address, in_flight_size: U256) {
        if in_flight_size > U256::ZERO {
            self.decrement_in_flight_order(user);
        }
    }

    pub(super) fn apply_in_flight_transition(
        &mut self,
        (user, was_in_flight, is_in_flight): (Address, bool, bool),
    ) {
        match (was_in_flight, is_in_flight) {
            (false, true) => {
                *self.in_flight_orders_by_user.entry(user).or_default() += 1;
            }
            (true, false) => self.decrement_in_flight_order(user),
            _ => {}
        }
    }

    fn decrement_in_flight_order(&mut self, user: Address) {
        if let Some(count) = self.in_flight_orders_by_user.get_mut(&user) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.in_flight_orders_by_user.remove(&user);
            }
        }
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
        let old_required = order_snapshot.reserved;
        let retained_required = self.in_flight_debit_for_order(&order_snapshot);
        let release = sub_or_zero(old_required, retained_required);

        if let Some(order) = self.orders.get_mut(order_id) {
            order.size = retained_size;
            order.reserved = retained_required;
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
