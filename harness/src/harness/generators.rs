use alloy::primitives::{Address, U256};
use rand::Rng;
use rand::rngs::SmallRng;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::service::{OrderType, Side};

const DECIMALS: u128 = 10u128.pow(18);

const MIN_TICK: u64 = 1;
const MAX_TICK: u64 = 100;
const INITIAL_TICK: u64 = 50;

pub const TICK_SIZE: U256 = U256::from_limbs([(DECIMALS / 100) as u64, (DECIMALS / 100 >> 64) as u64, 0, 0]);
pub const MIN_PRICE: U256 = U256::from_limbs([(DECIMALS / 100) as u64, (DECIMALS / 100 >> 64) as u64, 0, 0]);
pub const MAX_PRICE: U256 = U256::from_limbs([DECIMALS as u64, (DECIMALS >> 64) as u64, 0, 0]);

#[derive(Clone)]
pub struct FairPrice(Arc<AtomicU64>);

impl FairPrice {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU64::new(INITIAL_TICK)))
    }

    pub fn tick(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn drift(&self, rng: &mut SmallRng) {
        let step: i64 = rng.random_range(-1..=1);
        let _ = self.0.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            let next = (current as i64 + step).clamp(1, 99) as u64;
            Some(next)
        });
    }
}

pub fn tick_to_price(tick: u64) -> U256 {
    let clamped = tick.clamp(MIN_TICK, MAX_TICK) as u128;
    U256::from(clamped * DECIMALS / 100)
}

pub fn snap_to_tick(price: U256) -> U256 {
    let snapped = (price / TICK_SIZE) * TICK_SIZE;
    if snapped < MIN_PRICE {
        MIN_PRICE
    } else if snapped > MAX_PRICE {
        MAX_PRICE
    } else {
        snapped
    }
}

pub struct OrderParams {
    pub side: Side,
    pub order_type: OrderType,
    pub price: U256,
    pub size: U256,
}

pub enum ChainAction {
    TransferToUser(Address),
    WithdrawAll,
    NoOp,
}

pub fn pick_order_params(rng: &mut SmallRng, eoa_balance: U256, fair: &FairPrice) -> OrderParams {
    let mid = fair.tick();
    let is_market = rng.random_bool(0.30);
    let side = if rng.random_bool(0.5) { Side::Buy } else { Side::Sell };

    let price = if is_market {
        let slippage = rng.random_range(3..=8) as i64;
        let tick = match side {
            Side::Buy => (mid as i64 + slippage).clamp(MIN_TICK as i64, MAX_TICK as i64) as u64,
            Side::Sell => (mid as i64 - slippage).clamp(MIN_TICK as i64, MAX_TICK as i64) as u64,
        };
        tick_to_price(tick)
    } else {
        let offset = rng.random_range(1..=10) as i64;
        let tick = match side {
            Side::Buy => (mid as i64 - offset).clamp(MIN_TICK as i64, MAX_TICK as i64) as u64,
            Side::Sell => (mid as i64 + offset).clamp(MIN_TICK as i64, MAX_TICK as i64) as u64,
        };
        tick_to_price(tick)
    };

    let max_tokens = eoa_balance / U256::from(DECIMALS);
    let actual = if max_tokens.is_zero() { 1u128 } else { max_tokens.to::<u128>() };
    let max = actual.min(10_000);

    let roll: f64 = rng.random();
    let size_raw = if roll < 0.25 {
        // Oversized: 1.5-3x actual balance — candidate should reject
        let multiplier = rng.random_range(150u128..=300u128);
        (actual * multiplier / 100).max(1)
    } else if roll < 0.75 {
        rng.random_range(1..=max.min(100))
    } else {
        let lo = (max / 2).max(1);
        rng.random_range(lo..=max)
    };

    OrderParams {
        side,
        order_type: if is_market { OrderType::Market } else { OrderType::Limit },
        price,
        size: U256::from(size_raw * DECIMALS),
    }
}

pub fn pick_chain_action(
    rng: &mut SmallRng,
    self_index: usize,
    peers: &[Address],
    has_vault_balance: bool,
) -> ChainAction {
    let roll: f64 = rng.random();
    if has_vault_balance {
        match roll {
            x if x < 0.30 && peers.len() > 1 => {
                let mut idx = rng.random_range(0..peers.len());
                if idx == self_index {
                    idx = (idx + 1) % peers.len();
                }
                ChainAction::TransferToUser(peers[idx])
            }
            x if x < 0.60 => ChainAction::WithdrawAll,
            _ => ChainAction::NoOp,
        }
    } else {
        match roll {
            x if x < 0.35 && peers.len() > 1 => {
                let mut idx = rng.random_range(0..peers.len());
                if idx == self_index {
                    idx = (idx + 1) % peers.len();
                }
                ChainAction::TransferToUser(peers[idx])
            }
            _ => ChainAction::NoOp,
        }
    }
}
