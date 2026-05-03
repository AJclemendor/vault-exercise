use super::*;
use crate::chain::ChainClient;
use crate::engine::Engine;
use crate::sequencing::AdmissionSequencer;
use crate::types::{OrderType, Side, SubmitOrderRequest};
use alloy::primitives::{Address, U256};
use axum::Json;
use axum::extract::State;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

fn address(byte: u8) -> Address {
    Address::from([byte; 20])
}

fn wad(value: u64) -> U256 {
    U256::from(value) * U256::from(1_000_000_000_000_000_000u128)
}

fn test_chain_client() -> ChainClient {
    ChainClient::new(
        "http://127.0.0.1:1".into(),
        "0x0000000000000000000000000000000000000001".into(),
        "0x0000000000000000000000000000000000000002".into(),
        "0x1111111111111111111111111111111111111111111111111111111111111111".into(),
    )
    .expect("test chain client config should be valid")
}

#[tokio::test]
async fn queue_send_failure_cancels_submitted_order() {
    let buyer = address(1);
    let seller = address(2);
    let mut engine = Engine::new();
    engine.apply_balance_refresh(buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);
    engine
        .submit_order(SubmitOrderRequest {
            user: seller,
            side: Side::Sell,
            order_type: OrderType::Limit,
            price: wad(1),
            size: wad(1),
        })
        .expect("resting order should be accepted");

    let (settlement_queue, settlement_rx) = mpsc::unbounded_channel();
    drop(settlement_rx);
    let state = crate::AppState {
        engine: Arc::new(Mutex::new(engine)),
        chain: test_chain_client(),
        admission: Arc::new(AdmissionSequencer::new()),
        settlement_queue,
    };

    let result = submit_order(
        State(state.clone()),
        Ok(Json(SubmitOrderRequest {
            user: buyer,
            side: Side::Buy,
            order_type: OrderType::Limit,
            price: wad(1),
            size: wad(1),
        })),
    )
    .await;

    assert!(matches!(result, Err(ApiError::Chain(_))));
    let engine = state.engine.lock().await;
    assert!(engine.open_orders(Some(buyer)).is_empty());
    assert_eq!(engine.open_orders(Some(seller)).len(), 1);
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.fill_candidates, 1);
    assert_eq!(snapshot.settlements_aborted_before_tx, 1);
    assert_eq!(snapshot.settlement_pending_outcomes, 0);
}

#[tokio::test]
async fn invalid_order_is_rejected_before_balance_refresh() {
    let (settlement_queue, _settlement_rx) = mpsc::unbounded_channel();
    let state = crate::AppState {
        engine: Arc::new(Mutex::new(Engine::new())),
        chain: test_chain_client(),
        admission: Arc::new(AdmissionSequencer::new()),
        settlement_queue,
    };

    let result = submit_order(
        State(state.clone()),
        Ok(Json(SubmitOrderRequest {
            user: address(1),
            side: Side::Buy,
            order_type: OrderType::Limit,
            price: wad(1),
            size: U256::ZERO,
        })),
    )
    .await;

    assert!(matches!(result, Err(ApiError::BadRequest(_))));
    let engine = state.engine.lock().await;
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.orders_received, 1);
    assert_eq!(snapshot.orders_rejected_bad_request, 1);
}

#[test]
fn partial_queue_failure_only_aborts_unsent_fills() {
    let buyer = address(1);
    let first_seller = address(2);
    let second_seller = address(3);
    let mut engine = Engine::new();
    engine.apply_balance_refresh(buyer, wad(20), U256::ZERO);
    engine.apply_balance_refresh(first_seller, wad(10), U256::ZERO);
    engine.apply_balance_refresh(second_seller, wad(10), U256::ZERO);
    engine
        .submit_order(SubmitOrderRequest {
            user: first_seller,
            side: Side::Sell,
            order_type: OrderType::Limit,
            price: wad(1),
            size: wad(1),
        })
        .expect("first resting order should be accepted");
    engine
        .submit_order(SubmitOrderRequest {
            user: second_seller,
            side: Side::Sell,
            order_type: OrderType::Limit,
            price: wad(1),
            size: wad(1),
        })
        .expect("second resting order should be accepted");

    let admission = engine
        .submit_order_and_claim_fills(SubmitOrderRequest {
            user: buyer,
            side: Side::Buy,
            order_type: OrderType::Limit,
            price: wad(1),
            size: wad(2),
        })
        .expect("buyer order should be accepted");
    assert_eq!(admission.fills.len(), 2);

    engine.abort_admission_after_queue_failure(&admission.response.order_id, &admission.fills, 1);

    assert!(engine.fill_still_pending(&admission.fills[0]));
    assert!(!engine.fill_still_pending(&admission.fills[1]));
    assert_eq!(engine.open_orders(Some(buyer)).len(), 1);
    assert_eq!(engine.open_orders(Some(first_seller)).len(), 1);
    assert_eq!(engine.open_orders(Some(second_seller)).len(), 1);
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.fill_candidates, 2);
    assert_eq!(snapshot.settlements_aborted_before_tx, 1);
    assert_eq!(snapshot.settlement_pending_outcomes, 1);
}
