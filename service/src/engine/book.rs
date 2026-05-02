use alloy::primitives::U256;
use std::collections::HashMap;

use crate::types::{BookLevel, BookSnapshot, OrderType, Side};

use super::math::{format_wad, sub_or_zero};
use super::{Engine, Order};

impl Engine {
    pub(crate) fn book_snapshot(&self, depth: usize) -> BookSnapshot {
        let depth = depth.clamp(1, 100);
        let bid_levels = self.book_levels(Side::Buy, depth);
        let ask_levels = self.book_levels(Side::Sell, depth);

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

    fn book_levels(&self, side: Side, depth: usize) -> Vec<BookLevel> {
        let prices: Vec<_> = match side {
            Side::Buy => self.bids.keys().rev().copied().collect(),
            Side::Sell => self.asks.keys().copied().collect(),
        };
        let book = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        let mut levels = Vec::with_capacity(depth);

        for price in prices {
            let Some(queue) = book.get(&price) else {
                continue;
            };
            let mut size = U256::ZERO;
            let mut orders = 0;

            for order_id in queue {
                let Some(order) = self.orders.get(order_id) else {
                    continue;
                };
                if order.side == side
                    && order.price == price
                    && order.order_type == OrderType::Limit
                    && order.is_live()
                    && order.available_remaining() > U256::ZERO
                {
                    size += order.available_remaining();
                    orders += 1;
                }
            }

            if orders > 0 {
                levels.push(book_level(price, size, orders));
                if levels.len() == depth {
                    break;
                }
            }
        }

        levels
    }

    pub(super) fn index_limit_order(&mut self, side: Side, price: U256, order_id: String) {
        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        book.entry(price).or_default().push_back(order_id);
    }

    pub(super) fn available_limit_ids_at_price(&mut self, side: Side, price: U256) -> Vec<String> {
        self.clean_limit_level(side, price);

        let book = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        let Some(queue) = book.get(&price) else {
            return Vec::new();
        };

        queue
            .iter()
            .filter(|order_id| self.is_available_indexed_limit(order_id, side, price))
            .cloned()
            .collect()
    }

    fn clean_limit_level(&mut self, side: Side, price: U256) {
        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let Some(queue) = book.get_mut(&price) else {
            return;
        };
        let orders = &self.orders;
        queue.retain(|order_id| should_keep_indexed_limit(orders, order_id, side, price));
        if queue.is_empty() {
            book.remove(&price);
        }
    }

    fn is_available_indexed_limit(&self, order_id: &str, side: Side, price: U256) -> bool {
        self.orders
            .get(order_id)
            .map(|order| {
                order.side == side
                    && order.price == price
                    && order.order_type == OrderType::Limit
                    && order.is_available_for_fill()
            })
            .unwrap_or(false)
    }
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

fn should_keep_indexed_limit(
    orders: &HashMap<String, Order>,
    order_id: &str,
    side: Side,
    price: U256,
) -> bool {
    orders
        .get(order_id)
        .map(|order| {
            order.side == side
                && order.price == price
                && order.order_type == OrderType::Limit
                && order.is_live()
        })
        .unwrap_or(false)
}
