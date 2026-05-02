mod balances;
mod book;
mod matching;
mod math;
mod orders;
mod snapshot;

use crate::stats::Stats;
#[cfg(test)]
use crate::types::{ApiError, SubmitOrderRequest};
use crate::types::{OrderStatus, OrderType, OrderView, Side};
use alloy::primitives::{Address, U256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

#[cfg(test)]
use matching::limit_pair_priority;
#[cfg(test)]
use std::cmp::Ordering;

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
}

impl Default for BalanceState {
    fn default() -> Self {
        Self {
            real: U256::ZERO,
            reserved: U256::ZERO,
            vault: U256::ZERO,
            dirty: true,
            last_refresh: None,
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
        math::sub_or_zero(self.size, self.filled_size)
    }

    fn available_remaining(&self) -> U256 {
        math::sub_or_zero(self.total_remaining(), self.in_flight_size)
    }

    fn is_available_for_fill(&self) -> bool {
        self.is_live() && self.in_flight_size.is_zero() && self.total_remaining() > U256::ZERO
    }
}

#[derive(Debug)]
pub(crate) struct Engine {
    orders: HashMap<String, Order>,
    bids: BTreeMap<U256, VecDeque<String>>,
    asks: BTreeMap<U256, VecDeque<String>>,
    balances: HashMap<Address, BalanceState>,
    next_order_seq: u64,
    next_fill_seq: u64,
    stats: Stats,
}

impl Engine {
    pub(crate) fn new() -> Self {
        Self {
            orders: HashMap::new(),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            balances: HashMap::new(),
            next_order_seq: 1,
            next_fill_seq: 1,
            stats: Stats::default(),
        }
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

#[cfg(test)]
#[path = "../engine_tests.rs"]
mod tests;
