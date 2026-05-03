mod concurrency;
mod outcome;
mod requeue;

use crate::AppState;
use crate::engine::FillCandidate;
use crate::sequencing::OrderedGate;
use alloy::network::Ethereum;
use alloy::providers::PendingTransactionBuilder;
use concurrency::{PreSubmitReorderState, UserSettlementLocks};
use outcome::{
    confirm_and_apply_settlement, prepare_fill_for_submit, process_fill, refresh_for_settlement,
    spawn_receipt_task, submit_settlement_once,
};
use requeue::claim_and_enqueue_available_fills;
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const USER_SETTLEMENT_LOCK_STRIPES: usize = 256;
const DEFAULT_SETTLEMENT_MODE: &str = "receipt_concurrent";

type PendingSettlement = PendingTransactionBuilder<Ethereum>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettlementMode {
    Sequential,
    ReceiptConcurrent,
    Concurrent,
}

impl SettlementMode {
    fn default_mode() -> Self {
        Self::parse(DEFAULT_SETTLEMENT_MODE)
    }

    fn parse(value: &str) -> Self {
        match value {
            "sequential" => Self::Sequential,
            "receipt_concurrent" => Self::ReceiptConcurrent,
            "concurrent" => Self::Concurrent,
            _ => Self::ReceiptConcurrent,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::ReceiptConcurrent => "receipt_concurrent",
            Self::Concurrent => "concurrent",
        }
    }
}

#[derive(Debug, Clone)]
struct SettlementConfig {
    mode: SettlementMode,
    concurrency: usize,
    receipt_concurrency: usize,
    max_unresolved_settlements: usize,
    max_inflight_fills: usize,
    max_fill_claim_batch: usize,
}

impl SettlementConfig {
    fn from_env() -> Self {
        Self::from_sources(env::var("SETTLEMENT_MODE").ok().as_deref(), parse_usize_env)
    }

    fn from_sources(mode_value: Option<&str>, parse_usize: impl Fn(&str, usize) -> usize) -> Self {
        let mode = mode_value
            .map(SettlementMode::parse)
            .unwrap_or_else(SettlementMode::default_mode);
        let default_concurrency = if mode == SettlementMode::Concurrent {
            16
        } else {
            1
        };
        Self {
            mode,
            concurrency: parse_usize("SETTLEMENT_CONCURRENCY", default_concurrency).max(1),
            receipt_concurrency: parse_usize("SETTLEMENT_RECEIPT_CONCURRENCY", 64).max(1),
            max_unresolved_settlements: parse_usize("MAX_UNRESOLVED_SETTLEMENTS", 64).max(1),
            max_inflight_fills: parse_usize("MAX_INFLIGHT_FILLS", 64).max(1),
            max_fill_claim_batch: parse_usize("MAX_FILL_CLAIM_BATCH", 16).max(1),
        }
    }
}

fn parse_usize_env(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreSubmitDecision {
    Submit,
    Abort,
}

pub(super) enum SubmitOutcome {
    Submitted(PendingSettlement),
    SendFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PostSubmitFailurePolicy {
    ReleaseOrPrune,
    StaleBothOrders,
}

pub(crate) async fn settlement_loop(state: AppState, fill_rx: UnboundedReceiver<FillCandidate>) {
    let config = SettlementConfig::from_env();
    println!(
        "[config] settlement_mode={} settlement_concurrency={} settlement_receipt_concurrency={} max_unresolved_settlements={} max_inflight_fills={} max_fill_claim_batch={}",
        config.mode.as_str(),
        config.concurrency,
        config.receipt_concurrency,
        config.max_unresolved_settlements,
        config.max_inflight_fills,
        config.max_fill_claim_batch
    );

    match config.mode {
        SettlementMode::Sequential => sequential_settlement_loop(state, fill_rx).await,
        SettlementMode::ReceiptConcurrent => {
            receipt_concurrent_settlement_loop(state, fill_rx, config).await
        }
        SettlementMode::Concurrent => concurrent_settlement_loop(state, fill_rx, config).await,
    }
}

async fn sequential_settlement_loop(
    state: AppState,
    mut fill_rx: UnboundedReceiver<FillCandidate>,
) {
    while let Some(fill) = fill_rx.recv().await {
        process_fill(&state, &fill).await;
    }
}

async fn receipt_concurrent_settlement_loop(
    state: AppState,
    mut fill_rx: UnboundedReceiver<FillCandidate>,
    config: SettlementConfig,
) {
    let receipt_permits = Arc::new(Semaphore::new(config.receipt_concurrency));
    let unresolved_permits = Arc::new(Semaphore::new(config.max_unresolved_settlements));
    let apply_gate = Arc::new(OrderedGate::new(1));

    loop {
        let unresolved_permit = unresolved_permits
            .clone()
            .acquire_owned()
            .await
            .expect("settlement unresolved semaphore should not close");
        let fill = fill_rx.recv().await;
        let Some(fill) = fill else {
            drop(unresolved_permit);
            break;
        };

        match prepare_fill_for_submit(&state, &fill).await {
            PreSubmitDecision::Abort => {
                apply_gate.complete(fill.seq);
                drop(unresolved_permit);
                claim_and_enqueue_available_fills(&state).await;
                continue;
            }
            PreSubmitDecision::Submit => {}
        }

        match submit_settlement_once(&state, &fill).await {
            SubmitOutcome::SendFailed => {
                apply_gate.complete(fill.seq);
                drop(unresolved_permit);
                claim_and_enqueue_available_fills(&state).await;
            }
            SubmitOutcome::Submitted(pending) => {
                let receipt_permit = receipt_permits
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("settlement receipt semaphore should not close");
                spawn_receipt_task(
                    state.clone(),
                    fill,
                    pending,
                    apply_gate.clone(),
                    receipt_permit,
                    unresolved_permit,
                    PostSubmitFailurePolicy::ReleaseOrPrune,
                );
            }
        }
    }
}

async fn concurrent_settlement_loop(
    state: AppState,
    mut fill_rx: UnboundedReceiver<FillCandidate>,
    config: SettlementConfig,
) {
    let worker_permits = Arc::new(Semaphore::new(config.concurrency));
    let inflight_permits = Arc::new(Semaphore::new(config.max_inflight_fills));
    let receipt_permits = Arc::new(Semaphore::new(config.receipt_concurrency));
    let tx_gate = Arc::new(OrderedGate::new(1));
    let apply_gate = Arc::new(OrderedGate::new(1));
    let user_locks = Arc::new(UserSettlementLocks::new(USER_SETTLEMENT_LOCK_STRIPES));
    let reorder_state = Arc::new(PreSubmitReorderState::new());

    loop {
        let claim_generation = reorder_state.generation();
        let first_worker_permit = worker_permits
            .clone()
            .acquire_owned()
            .await
            .expect("settlement worker semaphore should not close");
        let first_inflight_permit = inflight_permits
            .clone()
            .acquire_owned()
            .await
            .expect("settlement in-flight semaphore should not close");
        let capacity = 1 + worker_permits
            .available_permits()
            .min(inflight_permits.available_permits())
            .min(config.max_fill_claim_batch.saturating_sub(1));
        let Some(first_fill) = fill_rx.recv().await else {
            drop(first_worker_permit);
            drop(first_inflight_permit);
            break;
        };
        let mut fills = Vec::with_capacity(capacity);
        fills.push(first_fill);
        while fills.len() < capacity {
            match fill_rx.try_recv() {
                Ok(fill) => fills.push(fill),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        let mut worker_permits_for_batch = Vec::with_capacity(fills.len());
        let mut inflight_permits_for_batch = Vec::with_capacity(fills.len());
        worker_permits_for_batch.push(first_worker_permit);
        inflight_permits_for_batch.push(first_inflight_permit);
        for _ in 1..fills.len() {
            worker_permits_for_batch.push(
                worker_permits
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("settlement worker semaphore should not close"),
            );
            inflight_permits_for_batch.push(
                inflight_permits
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("settlement in-flight semaphore should not close"),
            );
        }

        for ((fill, worker_permit), inflight_permit) in fills
            .into_iter()
            .zip(worker_permits_for_batch)
            .zip(inflight_permits_for_batch)
        {
            spawn_concurrent_settlement_worker(SettlementWorkerArgs {
                state: state.clone(),
                fill,
                tx_gate: tx_gate.clone(),
                apply_gate: apply_gate.clone(),
                receipt_permits: receipt_permits.clone(),
                user_locks: user_locks.clone(),
                reorder_state: reorder_state.clone(),
                claim_generation,
                worker_permit,
                inflight_permit,
            });
        }
    }
}

struct SettlementWorkerArgs {
    state: AppState,
    fill: FillCandidate,
    tx_gate: Arc<OrderedGate>,
    apply_gate: Arc<OrderedGate>,
    receipt_permits: Arc<Semaphore>,
    user_locks: Arc<UserSettlementLocks>,
    reorder_state: Arc<PreSubmitReorderState>,
    claim_generation: u64,
    worker_permit: OwnedSemaphorePermit,
    inflight_permit: OwnedSemaphorePermit,
}

fn spawn_concurrent_settlement_worker(args: SettlementWorkerArgs) {
    tokio::spawn(async move {
        let _worker_permit = args.worker_permit;
        let _inflight_permit = args.inflight_permit;
        let precheck = {
            let _user_guard = args
                .user_locks
                .lock_pair(args.fill.buyer, args.fill.seller)
                .await;
            precheck_fill_for_concurrent_submit(&args.state, &args.fill).await
        };

        let tx_turn = args.tx_gate.wait_for_turn(args.fill.seq).await;
        let _user_guard = args
            .user_locks
            .lock_pair(args.fill.buyer, args.fill.seller)
            .await;
        if args
            .reorder_state
            .invalidates(args.claim_generation, args.fill.seq)
        {
            let mut engine = args.state.engine.lock().await;
            if precheck != ConcurrentPreSubmitCheck::NotPending {
                engine.record_settlement_aborted_before_tx();
            }
            engine.abort_fill(&args.fill, false, false);
            drop(engine);
            args.reorder_state.record_event(args.fill.seq);
            drop(tx_turn);
            args.apply_gate.complete(args.fill.seq);
            drop(_user_guard);
            claim_and_enqueue_available_fills(&args.state).await;
            return;
        }
        if finalize_concurrent_pre_submit(&args.state, &args.fill, precheck).await
            == PreSubmitDecision::Abort
        {
            args.reorder_state.record_event(args.fill.seq);
            drop(tx_turn);
            args.apply_gate.complete(args.fill.seq);
            drop(_user_guard);
            claim_and_enqueue_available_fills(&args.state).await;
            return;
        }

        let submit_outcome = submit_settlement_once(&args.state, &args.fill).await;
        drop(tx_turn);

        match submit_outcome {
            SubmitOutcome::SendFailed => {
                args.reorder_state.record_event(args.fill.seq);
                args.apply_gate.complete(args.fill.seq);
                claim_and_enqueue_available_fills(&args.state).await;
            }
            SubmitOutcome::Submitted(pending) => {
                let _receipt_permit = args
                    .receipt_permits
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("settlement receipt semaphore should not close");
                confirm_and_apply_settlement(
                    args.state,
                    args.fill,
                    pending,
                    Some(args.apply_gate),
                    PostSubmitFailurePolicy::StaleBothOrders,
                )
                .await;
            }
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConcurrentPreSubmitCheck {
    Ready,
    RefreshFailed,
    NotPending,
    Underfunded,
}

async fn precheck_fill_for_concurrent_submit(
    state: &AppState,
    fill: &FillCandidate,
) -> ConcurrentPreSubmitCheck {
    {
        let mut engine = state.engine.lock().await;
        if !engine.fill_still_pending(fill) {
            engine.record_settlement_aborted_before_tx();
            return ConcurrentPreSubmitCheck::NotPending;
        }
        engine.record_settlement_attempted();
    }

    if let Err(err) = refresh_for_settlement(state, fill).await {
        eprintln!(
            "[settlement] refresh failed seq={} buy={} sell={} price={} size={}: {err:#}",
            fill.seq, fill.buyer, fill.seller, fill.exec_price, fill.fill_size
        );
        return ConcurrentPreSubmitCheck::RefreshFailed;
    }

    let mut engine = state.engine.lock().await;
    if !engine.fill_still_pending(fill) {
        engine.record_settlement_aborted_before_tx();
        return ConcurrentPreSubmitCheck::NotPending;
    }
    let (buyer_ok, seller_ok) = engine.users_funded_for_reserved(fill);
    if !buyer_ok || !seller_ok {
        return ConcurrentPreSubmitCheck::Underfunded;
    }

    ConcurrentPreSubmitCheck::Ready
}

async fn finalize_concurrent_pre_submit(
    state: &AppState,
    fill: &FillCandidate,
    precheck: ConcurrentPreSubmitCheck,
) -> PreSubmitDecision {
    match precheck {
        ConcurrentPreSubmitCheck::NotPending => return PreSubmitDecision::Abort,
        ConcurrentPreSubmitCheck::RefreshFailed => {
            let mut engine = state.engine.lock().await;
            engine.record_settlement_precheck_failed();
            engine.mark_dirty(fill.buyer);
            engine.mark_dirty(fill.seller);
            engine.abort_fill(fill, false, false);
            return PreSubmitDecision::Abort;
        }
        ConcurrentPreSubmitCheck::Ready | ConcurrentPreSubmitCheck::Underfunded => {}
    }

    let mut engine = state.engine.lock().await;
    if !engine.fill_still_pending(fill) {
        engine.record_settlement_aborted_before_tx();
        return PreSubmitDecision::Abort;
    }
    let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(fill);
    if !buyer_ok || !seller_ok {
        engine.record_settlement_precheck_failed();
        engine.abort_fill(fill, !buyer_ok, !seller_ok);
        return PreSubmitDecision::Abort;
    }
    engine.record_settlement_tx_attempt();
    PreSubmitDecision::Submit
}
