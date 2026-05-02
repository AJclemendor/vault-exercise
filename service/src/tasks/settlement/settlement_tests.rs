use super::*;
use crate::chain::SettlementConfirmationError;
use crate::engine::Engine;
use crate::types::{OrderStatus, OrderType, Side, SubmitOrderRequest};
use alloy::primitives::{Address, U256};

fn address(byte: u8) -> Address {
    Address::from([byte; 20])
}

fn wad(value: u64) -> U256 {
    U256::from(value) * U256::from(1_000_000_000_000_000_000u128)
}

fn submit(
    engine: &mut Engine,
    user: Address,
    side: Side,
    order_type: OrderType,
    price: U256,
    size: U256,
) -> String {
    engine
        .submit_order(SubmitOrderRequest {
            user,
            side,
            order_type,
            price,
            size,
        })
        .expect("order should be accepted")
        .order_id
}

#[test]
fn confirmation_failures_keep_receipt_errors_separate_from_reverts() {
    let mut engine = Engine::new();

    record_settlement_confirmation_failure(&mut engine, &SettlementConfirmationError::Reverted);
    record_settlement_confirmation_failure(
        &mut engine,
        &SettlementConfirmationError::Receipt(anyhow::anyhow!("receipt rpc timed out")),
    );

    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.settlements_reverted, 1);
    assert_eq!(snapshot.settlement_reverts, 1);
    assert_eq!(snapshot.settlement_tx_reverts, 1);
    assert_eq!(snapshot.settlement_receipt_status_reverted, 1);
    assert_eq!(snapshot.settlement_receipt_failures, 1);
    assert_eq!(snapshot.settlement_send_failures, 0);
    assert_eq!(snapshot.settlement_unknown_outcomes, 0);

    let json = serde_json::to_value(&snapshot).expect("stats snapshot should serialize");
    assert_eq!(json["settlement_reverts"], 1);
    assert_eq!(json["settlement_tx_reverts"], 1);
    assert_eq!(json["settlement_receipt_status_reverted"], 1);
    assert!(json.get("settlement_receipt_reverts").is_none());
}

#[test]
fn confirmation_receipt_failures_hold_uncertain_settlement_outcomes() {
    let err = SettlementConfirmationError::Receipt(anyhow::anyhow!("receipt rpc timed out"));

    assert_eq!(
        settlement_confirmation_failure_action(&err),
        SettlementFailureAction::HoldUncertainOutcome
    );
}

#[test]
fn unresolved_receipt_keeps_fill_pending_and_reservations_locked() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller = address(2);
    engine.apply_balance_refresh(buyer, wad(20), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);
    submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(10),
    );
    submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(10),
    );
    let fill = engine.next_fill_candidate().expect("orders should cross");
    let buyer_reserved = engine.balance_view(buyer).reserved;
    let seller_reserved = engine.balance_view(seller).reserved;

    hold_unresolved_settlement(
        &mut engine,
        &fill,
        &SettlementConfirmationError::Receipt(anyhow::anyhow!("receipt rpc timed out")),
    );

    assert!(engine.fill_still_pending(&fill));
    assert_eq!(engine.balance_view(buyer).reserved, buyer_reserved);
    assert_eq!(engine.balance_view(seller).reserved, seller_reserved);
    assert_eq!(engine.open_orders(Some(buyer))[0].status, OrderStatus::Open);
    assert!(engine.next_fill_candidate().is_none());
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.settlement_receipt_failures, 0);
    assert_eq!(snapshot.settlement_unknown_outcomes, 0);
    assert_eq!(snapshot.orders_marked_stale, 0);
}

#[test]
fn unresolved_receipt_timeout_stales_orders_and_releases_reservations() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller = address(2);
    engine.apply_balance_refresh(buyer, wad(20), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);
    submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(10),
    );
    submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(10),
    );
    let fill = engine.next_fill_candidate().expect("orders should cross");
    hold_unresolved_settlement(
        &mut engine,
        &fill,
        &SettlementConfirmationError::Receipt(anyhow::anyhow!("receipt rpc timed out")),
    );

    time_out_unresolved_settlement(&mut engine, &fill);

    assert!(!engine.fill_still_pending(&fill));
    assert!(engine.open_orders(Some(buyer)).is_empty());
    assert!(engine.open_orders(Some(seller)).is_empty());
    assert_eq!(engine.balance_view(buyer).reserved, U256::ZERO);
    assert_eq!(engine.balance_view(seller).reserved, U256::ZERO);
    assert!(engine.balance_view(buyer).stale);
    assert!(engine.balance_view(seller).stale);
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.settlement_receipt_failures, 1);
    assert_eq!(snapshot.settlement_unknown_outcomes, 1);
    assert_eq!(snapshot.orders_marked_stale, 2);
}

#[test]
fn confirmation_reverts_are_known_failures() {
    assert_eq!(
        settlement_confirmation_failure_action(&SettlementConfirmationError::Reverted),
        SettlementFailureAction::AbortKnownFailure
    );
}

#[test]
fn send_failures_abort_fill_state_without_tx_hash() {
    assert_eq!(
        settlement_send_failure_action(),
        SettlementFailureAction::AbortKnownFailure
    );
}
