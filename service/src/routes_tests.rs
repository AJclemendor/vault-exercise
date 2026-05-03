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
}
