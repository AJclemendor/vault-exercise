use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use std::collections::BTreeSet;
use tokio::sync::Notify;

#[derive(Debug, Clone)]
pub(crate) struct OrderedGate {
    inner: Arc<OrderedGateInner>,
}

#[derive(Debug)]
struct OrderedGateInner {
    state: Mutex<OrderedGateState>,
    notify: Notify,
}

#[derive(Debug)]
struct OrderedGateState {
    next_seq: u64,
    completed: BTreeSet<u64>,
}

impl OrderedGate {
    pub(crate) fn new(start: u64) -> Self {
        Self {
            inner: Arc::new(OrderedGateInner {
                state: Mutex::new(OrderedGateState {
                    next_seq: start,
                    completed: BTreeSet::new(),
                }),
                notify: Notify::new(),
            }),
        }
    }

    pub(crate) async fn wait_for_turn(&self, seq: u64) -> OrderedTurn {
        loop {
            let notified = self.inner.notify.notified();
            {
                let state = self.inner.state.lock().expect("ordered gate poisoned");
                if state.next_seq == seq {
                    return OrderedTurn {
                        inner: self.inner.clone(),
                        seq,
                        completed: false,
                    };
                }
                if state.next_seq > seq {
                    return OrderedTurn {
                        inner: self.inner.clone(),
                        seq,
                        completed: true,
                    };
                }
            }
            notified.await;
        }
    }

    pub(crate) fn complete(&self, seq: u64) {
        self.inner.complete(seq);
    }
}

#[derive(Debug)]
pub(crate) struct OrderedTurn {
    inner: Arc<OrderedGateInner>,
    seq: u64,
    completed: bool,
}

impl OrderedTurn {
    fn complete_inner(&mut self) {
        if self.completed {
            return;
        }
        self.inner.complete(self.seq);
        self.completed = true;
    }
}

impl Drop for OrderedTurn {
    fn drop(&mut self) {
        self.complete_inner();
    }
}

impl OrderedGateInner {
    fn complete(&self, seq: u64) {
        let mut state = self.state.lock().expect("ordered gate poisoned");
        if seq < state.next_seq {
            return;
        }

        state.completed.insert(seq);
        let mut advanced = false;
        loop {
            let next_seq = state.next_seq;
            if !state.completed.remove(&next_seq) {
                break;
            }
            state.next_seq += 1;
            advanced = true;
        }

        if advanced {
            self.notify.notify_waiters();
        }
    }
}

#[derive(Debug)]
pub(crate) struct AdmissionSequencer {
    next_ticket: AtomicU64,
    gate: OrderedGate,
}

impl AdmissionSequencer {
    pub(crate) fn new() -> Self {
        Self {
            next_ticket: AtomicU64::new(1),
            gate: OrderedGate::new(1),
        }
    }

    pub(crate) fn issue_ticket(&self) -> u64 {
        self.next_ticket.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) async fn wait_for_turn(&self, ticket: u64) -> AdmissionTurn {
        AdmissionTurn {
            _turn: self.gate.wait_for_turn(ticket).await,
        }
    }
}

#[derive(Debug)]
pub(crate) struct AdmissionTurn {
    _turn: OrderedTurn,
}

#[cfg(test)]
#[path = "sequencing_tests.rs"]
mod tests;
