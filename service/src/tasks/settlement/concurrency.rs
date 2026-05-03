use alloy::primitives::Address;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

const MAX_REORDER_EVENTS: usize = 1024;

#[derive(Debug)]
pub(super) struct PreSubmitReorderState {
    generation: AtomicU64,
    events: std::sync::Mutex<ReorderEvents>,
}

#[derive(Debug, Default)]
struct ReorderEvents {
    retained: VecDeque<(u64, u64)>,
    pruned_min_seq: Option<u64>,
    pruned_through_generation: u64,
}

impl PreSubmitReorderState {
    pub(super) fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            events: std::sync::Mutex::new(ReorderEvents::default()),
        }
    }

    pub(super) fn record_event(&self, seq: u64) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut events = self
            .events
            .lock()
            .expect("pre-submit reorder state poisoned");
        events.retained.push_back((generation, seq));
        while events.retained.len() > MAX_REORDER_EVENTS {
            let Some((pruned_generation, pruned_seq)) = events.retained.pop_front() else {
                break;
            };
            events.pruned_through_generation = pruned_generation;
            events.pruned_min_seq = Some(
                events
                    .pruned_min_seq
                    .map(|current| current.min(pruned_seq))
                    .unwrap_or(pruned_seq),
            );
        }
        generation
    }

    pub(super) fn invalidates(&self, claim_generation: u64, fill_seq: u64) -> bool {
        let events = self
            .events
            .lock()
            .expect("pre-submit reorder state poisoned");
        if claim_generation < events.pruned_through_generation
            && events
                .pruned_min_seq
                .map(|seq| seq < fill_seq)
                .unwrap_or(false)
        {
            return true;
        }
        events
            .retained
            .iter()
            .any(|(generation, seq)| *generation > claim_generation && *seq < fill_seq)
    }

    #[cfg(test)]
    pub(super) fn retained_event_count(&self) -> usize {
        self.events
            .lock()
            .expect("pre-submit reorder state poisoned")
            .retained
            .len()
    }
}

#[derive(Debug)]
pub(super) struct UserSettlementLocks {
    stripes: Vec<Arc<AsyncMutex<()>>>,
}

impl UserSettlementLocks {
    pub(super) fn new(stripes: usize) -> Self {
        Self {
            stripes: (0..stripes)
                .map(|_| Arc::new(AsyncMutex::new(())))
                .collect(),
        }
    }

    pub(super) async fn lock_pair(&self, first: Address, second: Address) -> UserSettlementGuard {
        let first_index = self.stripe_index(first);
        let second_index = self.stripe_index(second);
        if first_index == second_index {
            return UserSettlementGuard {
                _first: self.stripes[first_index].clone().lock_owned().await,
                _second: None,
            };
        }

        let (lower, upper) = if first_index < second_index {
            (first_index, second_index)
        } else {
            (second_index, first_index)
        };
        let first_guard = self.stripes[lower].clone().lock_owned().await;
        let second_guard = self.stripes[upper].clone().lock_owned().await;
        UserSettlementGuard {
            _first: first_guard,
            _second: Some(second_guard),
        }
    }

    fn stripe_index(&self, user: Address) -> usize {
        user.as_slice().iter().fold(0usize, |acc, byte| {
            acc.wrapping_mul(31) ^ usize::from(*byte)
        }) % self.stripes.len()
    }
}

#[derive(Debug)]
pub(super) struct UserSettlementGuard {
    _first: OwnedMutexGuard<()>,
    _second: Option<OwnedMutexGuard<()>>,
}
