use alloy::primitives::{Address, U256};
use std::cmp::Ordering;
use std::time::{Duration, Instant};

use crate::runtime::balance_tuning;
use crate::types::BalanceView;

use super::Engine;
use super::math::sub_or_zero;

impl Engine {
    pub(crate) fn balance_needs_admission_refresh(&self, user: Address) -> bool {
        let Some(balance) = self.balances.get(&user) else {
            return true;
        };
        if balance.dirty {
            return true;
        }
        let tuning = balance_tuning();
        match balance.last_refresh {
            Some(last) => last.elapsed() > tuning.admission_cache_max_age,
            None => true,
        }
    }

    #[cfg(test)]
    pub(crate) fn apply_balance_refresh(&mut self, user: Address, real: U256, vault: U256) {
        self.apply_balance_refresh_at_block(user, real, vault, u64::MAX);
    }

    pub(crate) fn apply_balance_refresh_at_block(
        &mut self,
        user: Address,
        real: U256,
        vault: U256,
        block: u64,
    ) {
        let balance = self.balances.entry(user).or_default();
        if balance
            .last_refresh_block
            .map(|last_block| block < last_block)
            .unwrap_or(false)
        {
            return;
        }

        balance.real = real;
        balance.vault = vault;
        if balance
            .dirty_after_block
            .map(|dirty_block| block >= dirty_block)
            .unwrap_or(true)
        {
            balance.dirty = false;
            balance.dirty_after_block = None;
        }
        balance.last_refresh = Some(Instant::now());
        balance.last_refresh_block = Some(block);
    }

    #[cfg(test)]
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

    pub(crate) fn balance_view_with_chain_values(
        &self,
        user: Address,
        real: U256,
        vault: U256,
    ) -> BalanceView {
        let reserved = self
            .balances
            .get(&user)
            .map(|balance| balance.reserved)
            .unwrap_or(U256::ZERO);
        let deficit = sub_or_zero(reserved, real);
        BalanceView {
            real,
            reserved,
            virtual_: sub_or_zero(real, reserved),
            deficit,
            over_reserved: deficit > U256::ZERO,
            vault,
            stale: false,
            last_refresh_age_ms: None,
        }
    }

    pub(crate) fn mark_dirty(&mut self, user: Address) {
        if let Some(balance) = self.balances.get_mut(&user)
            && !balance.dirty
        {
            balance.dirty = true;
            balance.dirty_after_block = None;
            self.stats.cache_dirty_events += 1;
        }
    }

    pub(crate) fn mark_dirty_at_block(&mut self, user: Address, block: u64) {
        let Some(balance) = self.balances.get_mut(&user) else {
            return;
        };
        if balance
            .last_refresh_block
            .map(|last_block| block <= last_block)
            .unwrap_or(false)
        {
            return;
        }
        if !balance.dirty {
            balance.dirty = true;
            self.stats.cache_dirty_events += 1;
        }
        balance.dirty_after_block = Some(
            balance
                .dirty_after_block
                .map(|current| current.max(block))
                .unwrap_or(block),
        );
    }

    pub(crate) fn refresh_candidates(&self, limit: usize) -> Vec<Address> {
        let now = Instant::now();
        let tuning = balance_tuning();
        let mut candidates: Vec<_> = self
            .balances
            .iter()
            .filter(|(_, balance)| balance.reserved > U256::ZERO)
            .filter(|(_, balance)| {
                balance.dirty
                    || balance
                        .last_refresh
                        .map(|last| now.duration_since(last) > tuning.active_cache_max_age)
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
        self.stale_over_reserved_orders_for_user(user, exact_order.as_deref());
    }
}
