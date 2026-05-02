use crate::types::Side;
use alloy::primitives::U256;

use super::WAD;

pub(super) fn sub_or_zero(left: U256, right: U256) -> U256 {
    if left > right {
        left - right
    } else {
        U256::ZERO
    }
}

pub(super) fn min_u256(left: U256, right: U256) -> U256 {
    if left <= right { left } else { right }
}

pub(super) fn format_wad(value: U256, decimals: usize) -> String {
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

pub(super) fn reservation_for(side: Side, price: U256, size: U256) -> Option<U256> {
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
