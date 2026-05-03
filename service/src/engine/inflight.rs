use alloy::primitives::U256;

use crate::types::OrderStatus;

use super::{Engine, FillCandidate};

impl Engine {
    pub(super) fn track_in_flight_fill(&mut self, candidate: &FillCandidate) {
        self.in_flight_fills
            .insert(candidate.seq, candidate.clone());
    }

    pub(super) fn untrack_in_flight_fill(&mut self, seq: u64) {
        self.in_flight_fills.remove(&seq);
    }

    pub(crate) fn advance_fill_claim_generation(&mut self, generation: u64) {
        self.fill_claim_generation = self.fill_claim_generation.max(generation);
    }

    pub(super) fn pop_pending_fill(&mut self) -> Option<FillCandidate> {
        while let Some(fill) = self.pending_fills.pop_front() {
            if self.fill_still_pending(&fill) {
                return Some(fill);
            }
        }
        None
    }

    pub(super) fn release_inflight(&mut self, order_id: &str, fill_size: U256) {
        let cancel_after_release = self.subtract_order_in_flight(order_id, fill_size);

        if cancel_after_release {
            self.terminal_order(order_id, OrderStatus::Cancelled);
        }
    }

    pub(super) fn release_related_in_flight_fills(&mut self, order_id: &str) {
        let related: Vec<_> = self
            .in_flight_fills
            .values()
            .filter(|fill| fill.buy_id == order_id || fill.sell_id == order_id)
            .cloned()
            .collect();

        for fill in related {
            self.in_flight_fills.remove(&fill.seq);
            if fill.buy_id != order_id {
                self.release_inflight(&fill.buy_id, fill.fill_size);
            }
            if fill.sell_id != order_id {
                self.release_inflight(&fill.sell_id, fill.fill_size);
            }
        }
    }
}
