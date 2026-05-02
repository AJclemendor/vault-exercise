use crate::stats::{Stats, StatsSnapshot, pct};
use crate::types::{
    ApiError, BalanceView, BookLevel, BookSnapshot, OrderResponse, OrderStatus, OrderType,
    OrderView, Side, SubmitOrderRequest,
};
use alloy::primitives::{Address, U256};
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

const WAD: u128 = 1_000_000_000_000_000_000;
const ADMISSION_CACHE_MAX_AGE: Duration = Duration::from_secs(3);
const ACTIVE_CACHE_MAX_AGE: Duration = Duration::from_millis(900);

#[derive(Debug, Clone)]
struct BalanceState {
    real: U256,
    reserved: U256,
    vault: U256,
    dirty: bool,
    last_refresh: Option<Instant>,
    last_activity: Instant,
}

impl Default for BalanceState {
    fn default() -> Self {
        Self {
            real: U256::ZERO,
            reserved: U256::ZERO,
            vault: U256::ZERO,
            dirty: true,
            last_refresh: None,
            last_activity: Instant::now(),
        }
    }
}

#[derive(Debug, Clone)]
struct Order {
    id: String,
    user: Address,
    side: Side,
    order_type: OrderType,
    price: U256,
    size: U256,
    filled_size: U256,
    in_flight_size: U256,
    status: OrderStatus,
    created_seq: u64,
    cancel_requested: bool,
    matched_once: bool,
}

impl Order {
    fn view(&self) -> OrderView {
        OrderView {
            id: self.id.clone(),
            user: self.user,
            side: self.side,
            order_type: self.order_type,
            price: self.price,
            size: self.size,
            filled_size: self.filled_size,
            status: self.status,
        }
    }

    fn is_live(&self) -> bool {
        matches!(
            self.status,
            OrderStatus::Open | OrderStatus::PartiallyFilled
        ) && self.total_remaining() > U256::ZERO
    }

    fn total_remaining(&self) -> U256 {
        sub_or_zero(self.size, self.filled_size)
    }

    fn available_remaining(&self) -> U256 {
        sub_or_zero(self.total_remaining(), self.in_flight_size)
    }

    fn is_available_for_fill(&self) -> bool {
        self.is_live() && self.in_flight_size.is_zero() && self.total_remaining() > U256::ZERO
    }
}

#[derive(Debug)]
pub(crate) struct Engine {
    orders: HashMap<String, Order>,
    balances: HashMap<Address, BalanceState>,
    next_order_seq: u64,
    next_fill_seq: u64,
    stats: Stats,
}

impl Engine {
    pub(crate) fn new() -> Self {
        Self {
            orders: HashMap::new(),
            balances: HashMap::new(),
            next_order_seq: 1,
            next_fill_seq: 1,
            stats: Stats::default(),
        }
    }

    pub(crate) fn balance_needs_admission_refresh(&self, user: Address) -> bool {
        let Some(balance) = self.balances.get(&user) else {
            return true;
        };
        if balance.dirty {
            return true;
        }
        match balance.last_refresh {
            Some(last) => last.elapsed() > ADMISSION_CACHE_MAX_AGE,
            None => true,
        }
    }

    pub(crate) fn apply_balance_refresh(&mut self, user: Address, real: U256, vault: U256) {
        let balance = self.balances.entry(user).or_default();
        balance.real = real;
        balance.vault = vault;
        balance.dirty = false;
        balance.last_refresh = Some(Instant::now());
    }

    pub(crate) fn submit_order(
        &mut self,
        request: SubmitOrderRequest,
    ) -> std::result::Result<OrderResponse, ApiError> {
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
        let balance = self.balances.entry(request.user).or_default();
        balance.last_activity = Instant::now();

        if balance.dirty || balance.last_refresh.is_none() {
            self.record_order_rejected_stale_balance_cache();
            return Err(ApiError::Chain(
                "balance cache is not fresh enough for admission".into(),
            ));
        }

        let virtual_balance = sub_or_zero(balance.real, balance.reserved);
        if virtual_balance < required {
            self.record_order_rejected_insufficient_balance();
            return Err(ApiError::BadRequest(format!(
                "insufficient available balance: required={required}, available={virtual_balance}"
            )));
        }

        balance.reserved += required;

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
        self.stats.orders_accepted += 1;

        Ok(OrderResponse {
            order_id: id,
            status: OrderStatus::Open,
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

    pub(crate) fn balance_view(&self, user: Address) -> BalanceView {
        let Some(balance) = self.balances.get(&user) else {
            return BalanceView {
                real: U256::ZERO,
                reserved: U256::ZERO,
                virtual_: U256::ZERO,
                vault: U256::ZERO,
            };
        };

        BalanceView {
            real: balance.real,
            reserved: balance.reserved,
            virtual_: sub_or_zero(balance.real, balance.reserved),
            vault: balance.vault,
        }
    }

    pub(crate) fn open_orders(&self, user: Option<Address>) -> Vec<OrderView> {
        let mut orders: Vec<_> = self
            .orders
            .values()
            .filter(|order| order.is_live())
            .filter(|order| user.map(|u| order.user == u).unwrap_or(true))
            .map(Order::view)
            .collect();
        orders.sort_by_key(|order| order.id.clone());
        orders
    }

    pub(crate) fn book_snapshot(&self, depth: usize) -> BookSnapshot {
        let depth = depth.clamp(1, 100);
        let mut bids: BTreeMap<U256, (U256, usize)> = BTreeMap::new();
        let mut asks: BTreeMap<U256, (U256, usize)> = BTreeMap::new();

        for order in self.orders.values().filter(|order| {
            order.is_live()
                && order.order_type == OrderType::Limit
                && order.available_remaining() > U256::ZERO
        }) {
            let levels = match order.side {
                Side::Buy => &mut bids,
                Side::Sell => &mut asks,
            };
            let entry = levels.entry(order.price).or_insert((U256::ZERO, 0));
            entry.0 += order.available_remaining();
            entry.1 += 1;
        }

        let bid_levels: Vec<_> = bids
            .iter()
            .rev()
            .take(depth)
            .map(|(price, (size, orders))| book_level(*price, *size, *orders))
            .collect();
        let ask_levels: Vec<_> = asks
            .iter()
            .take(depth)
            .map(|(price, (size, orders))| book_level(*price, *size, *orders))
            .collect();

        let best_bid_raw = bid_levels.first().map(|level| level.price_raw);
        let best_ask_raw = ask_levels.first().map(|level| level.price_raw);
        let spread_raw = match (best_bid_raw, best_ask_raw) {
            (Some(bid), Some(ask)) => Some(sub_or_zero(ask, bid)),
            _ => None,
        };
        let mid_raw = match (best_bid_raw, best_ask_raw) {
            (Some(bid), Some(ask)) => Some((bid + ask) / U256::from(2u8)),
            _ => None,
        };

        BookSnapshot {
            depth,
            best_bid: best_bid_raw.map(|price| format_wad(price, 4)),
            best_bid_raw,
            best_ask: best_ask_raw.map(|price| format_wad(price, 4)),
            best_ask_raw,
            spread: spread_raw.map(|spread| format_wad(spread, 4)),
            spread_raw,
            mid: mid_raw.map(|mid| format_wad(mid, 4)),
            mid_raw,
            bids: bid_levels,
            asks: ask_levels,
        }
    }

    pub(crate) fn stats_snapshot(&self) -> StatsSnapshot {
        let live_orders = self.orders.values().filter(|order| order.is_live()).count();
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

        StatsSnapshot {
            orders_received: self.stats.orders_received,
            orders_accepted: self.stats.orders_accepted,
            orders_accepted_pct: pct(self.stats.orders_accepted, self.stats.orders_received),
            orders_rejected: self.stats.orders_rejected,
            orders_rejected_pct: pct(self.stats.orders_rejected, self.stats.orders_received),
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
            orders_matched: self.stats.orders_matched,
            orders_with_successful_fill: self.stats.orders_matched,
            order_sides_filled: self.stats.order_sides_filled,
            fill_candidates: self.stats.fill_candidates,
            orders_matched_pct_of_accepted: pct(
                self.stats.orders_matched,
                self.stats.orders_accepted,
            ),
            settlements_attempted: self.stats.settlements_attempted,
            settlement_tx_attempts: self.stats.settlement_tx_attempts,
            settlements_precheck_failed: self.stats.settlements_precheck_failed,
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
            settlement_receipt_reverts: self.stats.settlements_reverted,
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

    pub(crate) fn record_pre_settlement_balance_refreshes(&mut self, count: u64) {
        self.stats.pre_settlement_balance_refreshes += count;
    }

    pub(crate) fn record_background_balance_refresh(&mut self) {
        self.stats.background_balance_refreshes += 1;
    }

    pub(crate) fn mark_dirty(&mut self, user: Address) {
        if let Some(balance) = self.balances.get_mut(&user)
            && !balance.dirty
        {
            balance.dirty = true;
            self.stats.cache_dirty_events += 1;
        }
    }

    pub(crate) fn refresh_candidates(&self, limit: usize) -> Vec<Address> {
        let now = Instant::now();
        let mut candidates: Vec<_> = self
            .balances
            .iter()
            .filter(|(_, balance)| balance.reserved > U256::ZERO)
            .filter(|(_, balance)| {
                balance.dirty
                    || balance
                        .last_refresh
                        .map(|last| now.duration_since(last) > ACTIVE_CACHE_MAX_AGE)
                        .unwrap_or(true)
            })
            .map(|(user, balance)| {
                let age = balance
                    .last_refresh
                    .map(|last| now.duration_since(last))
                    .unwrap_or(Duration::MAX);
                (*user, balance.dirty, age)
            })
            .collect();

        candidates.sort_by(|a, b| match (a.1, b.1) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => b.2.cmp(&a.2),
        });
        candidates
            .into_iter()
            .take(limit)
            .map(|(user, _, _)| user)
            .collect()
    }

    pub(crate) fn next_fill_candidate(&mut self) -> Option<FillCandidate> {
        loop {
            if let Some(market_id) = self.oldest_market_order() {
                if let Some(counterparty_id) = self.find_market_counterparty(&market_id) {
                    return self.prepare_fill(&market_id, &counterparty_id);
                }

                self.terminal_order(&market_id, OrderStatus::Cancelled);
                continue;
            }

            let (buy_id, sell_id) = self.best_crossing_limits()?;
            return self.prepare_fill(&buy_id, &sell_id);
        }
    }

    fn oldest_market_order(&self) -> Option<String> {
        self.orders
            .values()
            .filter(|order| order.is_available_for_fill() && order.order_type == OrderType::Market)
            .min_by_key(|order| order.created_seq)
            .map(|order| order.id.clone())
    }

    fn find_market_counterparty(&self, market_id: &str) -> Option<String> {
        let market = self.orders.get(market_id)?;

        match market.side {
            Side::Buy => self
                .orders
                .values()
                .filter(|order| {
                    order.is_live()
                        && order.order_type == OrderType::Limit
                        && order.side == Side::Sell
                        && order.user != market.user
                        && order.created_seq < market.created_seq
                        && order.is_available_for_fill()
                        && order.price <= market.price
                })
                .min_by(|a, b| match a.price.cmp(&b.price) {
                    Ordering::Equal => a.created_seq.cmp(&b.created_seq),
                    ordering => ordering,
                })
                .map(|order| order.id.clone()),
            Side::Sell => self
                .orders
                .values()
                .filter(|order| {
                    order.is_live()
                        && order.order_type == OrderType::Limit
                        && order.side == Side::Buy
                        && order.user != market.user
                        && order.created_seq < market.created_seq
                        && order.is_available_for_fill()
                        && order.price >= market.price
                })
                .max_by(|a, b| match a.price.cmp(&b.price) {
                    Ordering::Equal => b.created_seq.cmp(&a.created_seq),
                    ordering => ordering,
                })
                .map(|order| order.id.clone()),
        }
    }

    fn best_crossing_limits(&self) -> Option<(String, String)> {
        let mut best: Option<(&Order, &Order)> = None;

        for buy in self.orders.values().filter(|order| {
            order.is_available_for_fill()
                && order.order_type == OrderType::Limit
                && order.side == Side::Buy
        }) {
            for sell in self.orders.values().filter(|order| {
                order.is_available_for_fill()
                    && order.order_type == OrderType::Limit
                    && order.side == Side::Sell
                    && order.user != buy.user
                    && buy.price >= order.price
            }) {
                let replace = match best {
                    None => true,
                    Some((best_buy, best_sell)) => {
                        limit_pair_priority((buy, sell), (best_buy, best_sell)) == Ordering::Less
                    }
                };
                if replace {
                    best = Some((buy, sell));
                }
            }
        }

        best.map(|(buy, sell)| (buy.id.clone(), sell.id.clone()))
    }

    fn prepare_fill(&mut self, first_id: &str, second_id: &str) -> Option<FillCandidate> {
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
        if !buy.is_available_for_fill() || !sell.is_available_for_fill() {
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

        if let Some(order) = self.orders.get_mut(&buy_id) {
            order.in_flight_size += fill_size;
        }
        if let Some(order) = self.orders.get_mut(&sell_id) {
            order.in_flight_size += fill_size;
        }

        self.stats.fill_candidates += 1;
        Some(candidate)
    }

    pub(crate) fn fill_still_pending(&self, fill: &FillCandidate) -> bool {
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
        let buyer_ok = self
            .balances
            .get(&fill.buyer)
            .map(|balance| balance.real >= balance.reserved)
            .unwrap_or(false);
        let seller_ok = self
            .balances
            .get(&fill.seller)
            .map(|balance| balance.real >= balance.reserved)
            .unwrap_or(false);
        (buyer_ok, seller_ok)
    }

    pub(crate) fn prune_underfunded_fill_users(&mut self, fill: &FillCandidate) -> (bool, bool) {
        let (buyer_ok, seller_ok) = self.users_funded_for_reserved(fill);
        if !buyer_ok {
            self.prune_user_to_balance(fill.buyer, None);
        }
        if !seller_ok {
            self.prune_user_to_balance(fill.seller, None);
        }
        self.users_funded_for_reserved(fill)
    }

    pub(crate) fn apply_settlement_success(&mut self, fill: &FillCandidate) {
        if !self.fill_still_pending(fill) {
            return;
        }

        let matched_orders = self.apply_order_fill(&fill.buy_id, fill.fill_size) as u64
            + self.apply_order_fill(&fill.sell_id, fill.fill_size) as u64;

        self.stats.orders_matched += matched_orders;
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
        let release_after_fill: U256;
        let user;
        let first_successful_fill;

        {
            let Some(order) = self.orders.get_mut(order_id) else {
                return false;
            };
            user = order.user;
            first_successful_fill = !order.matched_once;
            order.matched_once = true;
            order.in_flight_size = sub_or_zero(order.in_flight_size, fill_size);
            order.filled_size += fill_size;

            if order.filled_size >= order.size {
                order.filled_size = order.size;
                order.status = OrderStatus::Filled;
                order.cancel_requested = false;
            } else {
                order.status = OrderStatus::PartiallyFilled;
                cancel_after_fill = order.cancel_requested;
            }

            let new_required = if order.is_live() {
                reservation_for(order.side, order.price, order.total_remaining())
                    .expect("stored order reservation should be bounded")
            } else {
                U256::ZERO
            };
            release_after_fill = sub_or_zero(old_required, new_required);
        }

        if release_after_fill > U256::ZERO {
            self.release_user_reservation(user, release_after_fill);
        }

        if cancel_after_fill {
            self.terminal_order(order_id, OrderStatus::Cancelled);
        }

        first_successful_fill
    }

    pub(crate) fn abort_fill(&mut self, fill: &FillCandidate, stale_buy: bool, stale_sell: bool) {
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

    fn release_inflight(&mut self, order_id: &str, fill_size: U256) {
        let mut cancel_after_release = false;
        if let Some(order) = self.orders.get_mut(order_id) {
            order.in_flight_size = sub_or_zero(order.in_flight_size, fill_size);
            cancel_after_release = order.cancel_requested && order.in_flight_size.is_zero();
        }

        if cancel_after_release {
            self.terminal_order(order_id, OrderStatus::Cancelled);
        }
    }

    pub(crate) fn prune_user_to_balance(&mut self, user: Address, exact_order: Option<String>) {
        let Some(balance) = self.balances.get(&user) else {
            return;
        };
        if balance.real >= balance.reserved {
            return;
        }

        if let Some(order_id) = exact_order {
            self.terminal_order(&order_id, OrderStatus::Stale);
            if self
                .balances
                .get(&user)
                .map(|balance| balance.real >= balance.reserved)
                .unwrap_or(true)
            {
                return;
            }
        }

        let mut candidates: Vec<_> = self
            .orders
            .values()
            .filter(|order| {
                order.user == user
                    && order.is_live()
                    && order.in_flight_size.is_zero()
                    && order.total_remaining() > U256::ZERO
            })
            .map(|order| (order.created_seq, order.id.clone()))
            .collect();
        candidates.sort_by_key(|candidate| Reverse(candidate.0));

        for (_, order_id) in candidates {
            self.terminal_order(&order_id, OrderStatus::Stale);
            if self
                .balances
                .get(&user)
                .map(|balance| balance.real >= balance.reserved)
                .unwrap_or(true)
            {
                break;
            }
        }
    }

    fn terminal_order(&mut self, order_id: &str, status: OrderStatus) {
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

    fn release_user_reservation(&mut self, user: Address, amount: U256) {
        let balance = self.balances.entry(user).or_default();
        balance.reserved = sub_or_zero(balance.reserved, amount);
        balance.last_activity = Instant::now();
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FillCandidate {
    pub(crate) seq: u64,
    pub(crate) buy_id: String,
    pub(crate) sell_id: String,
    pub(crate) buyer: Address,
    pub(crate) seller: Address,
    pub(crate) fill_size: U256,
    pub(crate) exec_price: U256,
    pub(crate) quote: U256,
    pub(crate) base: U256,
}

fn sub_or_zero(left: U256, right: U256) -> U256 {
    if left > right {
        left - right
    } else {
        U256::ZERO
    }
}

fn min_u256(left: U256, right: U256) -> U256 {
    if left <= right { left } else { right }
}

fn book_level(price: U256, size: U256, orders: usize) -> BookLevel {
    BookLevel {
        price: format_wad(price, 4),
        price_raw: price,
        size: format_wad(size, 2),
        size_raw: size,
        orders,
    }
}

fn format_wad(value: U256, decimals: usize) -> String {
    let decimals = decimals.min(18);
    let scale = U256::from(WAD);
    let whole = value / scale;
    if decimals == 0 {
        return whole.to_string();
    }

    let remainder = value % scale;
    let fraction = format!("{:018}", remainder.to::<u128>());
    let mut fraction = fraction[..decimals].to_string();
    while fraction.ends_with('0') {
        fraction.pop();
    }

    if fraction.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{fraction}")
    }
}

fn reservation_for(side: Side, price: U256, size: U256) -> Option<U256> {
    match side {
        Side::Buy => ceil_mul_div(price, size, U256::from(WAD)),
        Side::Sell => Some(size),
    }
}

fn ceil_mul_div(left: U256, right: U256, denominator: U256) -> Option<U256> {
    if denominator.is_zero() {
        return None;
    }
    let product = left.checked_mul(right)?;
    if product.is_zero() {
        return Some(U256::ZERO);
    }
    Some(((product - U256::from(1u8)) / denominator) + U256::from(1u8))
}

fn execution_price(buy: &Order, sell: &Order) -> U256 {
    match (buy.order_type, sell.order_type) {
        (OrderType::Market, OrderType::Limit) => sell.price,
        (OrderType::Limit, OrderType::Market) => buy.price,
        _ if buy.created_seq > sell.created_seq => sell.price,
        _ => buy.price,
    }
}

fn limit_pair_priority(candidate: (&Order, &Order), current: (&Order, &Order)) -> Ordering {
    let (candidate_buy, candidate_sell) = candidate;
    let (current_buy, current_sell) = current;

    // Better bid, then better ask, then FIFO within each side's price level.
    match current_buy.price.cmp(&candidate_buy.price) {
        Ordering::Equal => {}
        ordering => return ordering,
    }

    match candidate_sell.price.cmp(&current_sell.price) {
        Ordering::Equal => {}
        ordering => return ordering,
    }

    match candidate_buy.created_seq.cmp(&current_buy.created_seq) {
        Ordering::Equal => {}
        ordering => return ordering,
    }

    candidate_sell.created_seq.cmp(&current_sell.created_seq)
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
