use super::*;
use crate::engine::Engine;
use crate::types::{OrderType, Side, SubmitOrderRequest};
use alloy::primitives::{Address, U256};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, timeout};

fn address(byte: u8) -> Address {
    Address::from([byte; 20])
}

fn wad(value: u64) -> U256 {
    U256::from(value) * U256::from(1_000_000_000_000_000_000u128)
}

#[tokio::test]
async fn ordered_gate_runs_ready_tasks_in_sequence() {
    let gate = OrderedGate::new(1);
    let (tx, mut rx) = mpsc::unbounded_channel();

    for seq in [3, 1, 2] {
        let gate = gate.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let _turn = gate.wait_for_turn(seq).await;
            tx.send(seq).expect("receiver should be alive");
        });
    }
    drop(tx);

    assert_eq!(rx.recv().await, Some(1));
    assert_eq!(rx.recv().await, Some(2));
    assert_eq!(rx.recv().await, Some(3));
}

#[tokio::test]
async fn ordered_gate_does_not_unblock_gap() {
    let gate = OrderedGate::new(1);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let seq_two_gate = gate.clone();

    tokio::spawn(async move {
        let _turn = seq_two_gate.wait_for_turn(2).await;
        tx.send(2).expect("receiver should be alive");
    });

    gate.complete(3);
    assert!(timeout(Duration::from_millis(25), rx.recv()).await.is_err());

    gate.complete(1);
    assert_eq!(rx.recv().await, Some(2));
}

#[tokio::test]
async fn ordered_gate_completion_is_idempotent() {
    let gate = OrderedGate::new(1);
    let turn = gate.wait_for_turn(1).await;
    drop(turn);
    gate.complete(1);

    let _turn = gate.wait_for_turn(2).await;
}

#[tokio::test]
async fn later_receipt_ready_waits_for_earlier_apply() {
    let gate = OrderedGate::new(1);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let later_gate = gate.clone();
    tokio::spawn(async move {
        let _turn = later_gate.wait_for_turn(2).await;
        tx.send(2).expect("receiver should be alive");
    });

    assert!(timeout(Duration::from_millis(25), rx.recv()).await.is_err());

    let first_turn = gate.wait_for_turn(1).await;
    assert!(timeout(Duration::from_millis(25), rx.recv()).await.is_err());
    drop(first_turn);

    assert_eq!(rx.recv().await, Some(2));
}

#[tokio::test]
async fn admission_sequencer_commits_in_ticket_order() {
    let sequencer = Arc::new(AdmissionSequencer::new());
    let committed = Arc::new(Mutex::new(Vec::new()));
    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel();

    for label in ["slow-first", "fast-second"] {
        let ticket = sequencer.issue_ticket();
        let committed = committed.clone();
        let ready_tx = ready_tx.clone();
        tokio::spawn(async move {
            ready_tx.send(label).expect("receiver should be alive");
            if label == "slow-first" {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            let _turn = ticket.wait_for_turn().await;
            committed.lock().await.push(label);
        });
    }
    drop(ready_tx);

    assert_eq!(ready_rx.recv().await, Some("slow-first"));
    assert_eq!(ready_rx.recv().await, Some("fast-second"));
    tokio::time::sleep(Duration::from_millis(60)).await;

    assert_eq!(
        committed.lock().await.as_slice(),
        ["slow-first", "fast-second"]
    );
}

#[tokio::test]
async fn admission_refresh_failure_does_not_skip_sequence() {
    let sequencer = AdmissionSequencer::new();
    let failed_ticket = sequencer.issue_ticket();
    let next_ticket = sequencer.issue_ticket();

    {
        let _turn = failed_ticket.wait_for_turn().await;
    }

    let _turn = next_ticket.wait_for_turn().await;
}

#[tokio::test]
async fn dropped_queued_admission_ticket_does_not_block_later_ticket() {
    let sequencer = AdmissionSequencer::new();
    let blocker = sequencer.issue_ticket();
    let cancelled = sequencer.issue_ticket();
    let later = sequencer.issue_ticket();

    let cancelled_task = tokio::spawn(async move {
        let _turn = cancelled.wait_for_turn().await;
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    cancelled_task.abort();
    assert!(
        cancelled_task
            .await
            .expect_err("task should be cancelled")
            .is_cancelled()
    );

    drop(blocker);
    let _turn = timeout(Duration::from_millis(25), later.wait_for_turn())
        .await
        .expect("later ticket should not wedge behind cancelled ticket");
}

#[tokio::test]
async fn submit_order_created_seq_follows_admission_ticket_order() {
    let sequencer = Arc::new(AdmissionSequencer::new());
    let engine = Arc::new(Mutex::new(Engine::new()));
    let first = address(1);
    let second = address(2);
    {
        let mut engine = engine.lock().await;
        engine.apply_balance_refresh(first, wad(10), U256::ZERO);
        engine.apply_balance_refresh(second, wad(10), U256::ZERO);
    }

    let first_ticket = sequencer.issue_ticket();
    let second_ticket = sequencer.issue_ticket();

    let second_task = {
        let engine = engine.clone();
        tokio::spawn(async move {
            let _turn = second_ticket.wait_for_turn().await;
            engine
                .lock()
                .await
                .submit_order(SubmitOrderRequest {
                    user: second,
                    side: Side::Sell,
                    order_type: OrderType::Limit,
                    price: wad(1),
                    size: wad(1),
                })
                .expect("second order should be accepted")
                .order_id
        })
    };

    tokio::time::sleep(Duration::from_millis(25)).await;
    let first_id = {
        let _turn = first_ticket.wait_for_turn().await;
        engine
            .lock()
            .await
            .submit_order(SubmitOrderRequest {
                user: first,
                side: Side::Buy,
                order_type: OrderType::Limit,
                price: wad(1),
                size: wad(1),
            })
            .expect("first order should be accepted")
            .order_id
    };
    let second_id = second_task.await.expect("second task should complete");

    assert_eq!(first_id, "ord-1");
    assert_eq!(second_id, "ord-2");
}
