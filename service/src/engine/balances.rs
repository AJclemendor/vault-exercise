use alloy::primitives::{Address, U256};
use std::cmp::{Ordering, Reverse};
use std::time::{Duration, Instant};

use crate::types::{BalanceView, OrderStatus};

use super::math::sub_or_zero;
use super::{ACTIVE_CACHE_MAX_AGE, ADMISSION_CACHE_MAX_AGE, Engine};

impl Engine {
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

    pub(crate) fn balance_view(&self, user: Address) -> BalanceView {
        let Some(balance) = self.balances.get(&user) else {
            return BalanceView {
                real: U256::ZERO,
                reserved: U256::ZERO,
                virtual_: U256::ZERO,
                deficit: U256::ZERO,
                over_reserved: false,
                vault: U256::ZERO,
                stale: true,
                last_refresh_age_ms: None,
            };
        };

        let deficit = sub_or_zero(balance.reserved, balance.real);
        BalanceView {
            real: balance.real,
            reserved: balance.reserved,
            virtual_: sub_or_zero(balance.real, balance.reserved),
            deficit,
            over_reserved: deficit > U256::ZERO,
            vault: balance.vault,
            stale: balance.dirty || balance.last_refresh.is_none(),
            last_refresh_age_ms: balance
                .last_refresh
                .map(|last| last.elapsed().as_millis() as u64),
        }
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
}
