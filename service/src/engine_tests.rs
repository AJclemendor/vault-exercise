use super::*;

fn address(byte: u8) -> Address {
    Address::from([byte; 20])
}

fn wad(value: u64) -> U256 {
    U256::from(value) * U256::from(WAD)
}

fn order(side: Side, price: u64, created_seq: u64) -> Order {
    Order {
        id: format!("ord-{created_seq}"),
        user: Address::from([created_seq as u8; 20]),
        side,
        order_type: OrderType::Limit,
        price: U256::from(price),
        size: U256::from(1u8),
        filled_size: U256::ZERO,
        in_flight_size: U256::ZERO,
        status: OrderStatus::Open,
        created_seq,
        cancel_requested: false,
        matched_once: false,
    }
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
fn mark_dirty_invalidates_unreserved_cached_user() {
    let mut engine = Engine::new();
    let user = address(1);

    engine.apply_balance_refresh(user, wad(100), U256::ZERO);
    assert!(!engine.balance_needs_admission_refresh(user));

    engine.mark_dirty(user);

    assert!(engine.balance_needs_admission_refresh(user));
    assert_eq!(engine.stats.cache_dirty_events, 1);
}

#[test]
fn market_order_does_not_match_counterparty_created_later() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller = address(2);
    engine.apply_balance_refresh(buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);

    let market_id = submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Market,
        wad(1),
        wad(10),
    );
    let sell_id = submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(10),
    );

    assert!(engine.next_fill_candidate().is_none());
    assert_eq!(engine.orders[&market_id].status, OrderStatus::Cancelled);
    assert_eq!(engine.orders[&sell_id].status, OrderStatus::Open);
    assert_eq!(engine.stats.fill_candidates, 0);
}

#[test]
fn precheck_prunes_newer_reservations_before_staling_inflight_order() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller = address(2);
    engine.apply_balance_refresh(buyer, wad(20), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);

    let older_buy_id = submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(10),
    );
    let newer_buy_id = submit(
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
    let fill = engine
        .next_fill_candidate()
        .expect("older buy should cross the sell");
    assert_eq!(fill.buy_id, older_buy_id);

    engine.apply_balance_refresh(buyer, wad(10), U256::ZERO);
    let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(&fill);

    assert!(buyer_ok);
    assert!(seller_ok);
    assert!(engine.fill_still_pending(&fill));
    assert_eq!(engine.orders[&older_buy_id].status, OrderStatus::Open);
    assert_eq!(engine.orders[&newer_buy_id].status, OrderStatus::Stale);
}

#[test]
fn settlement_success_uses_refreshed_chain_balances_and_counts_matched_orders_once() {
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
    assert_eq!(engine.stats.fill_candidates, 1);
    assert_eq!(engine.stats.orders_matched, 0);

    engine.apply_balance_refresh(buyer, wad(10), wad(10));
    engine.apply_balance_refresh(seller, U256::ZERO, wad(10));
    engine.apply_settlement_success(&fill);

    let buyer_balance = engine.balance_view(buyer);
    let seller_balance = engine.balance_view(seller);
    assert_eq!(buyer_balance.real, wad(10));
    assert_eq!(buyer_balance.vault, wad(10));
    assert_eq!(buyer_balance.reserved, U256::ZERO);
    assert_eq!(seller_balance.real, U256::ZERO);
    assert_eq!(seller_balance.vault, wad(10));
    assert_eq!(seller_balance.reserved, U256::ZERO);
    assert_eq!(engine.stats.orders_matched, 2);
    assert_eq!(engine.stats.order_sides_filled, 2);
    assert_eq!(engine.stats.successful_settlements, 1);
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.orders_with_successful_fill, 2);
    assert_eq!(snapshot.order_sides_filled, 2);
    assert_eq!(snapshot.fills_settled, 1);
}

#[test]
fn repeated_partial_fills_do_not_double_count_the_same_matched_order() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller_one = address(2);
    let seller_two = address(3);
    engine.apply_balance_refresh(buyer, wad(100), U256::ZERO);
    engine.apply_balance_refresh(seller_one, wad(20), U256::ZERO);
    engine.apply_balance_refresh(seller_two, wad(20), U256::ZERO);

    let buyer_id = submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(50),
    );
    let seller_one_id = submit(
        &mut engine,
        seller_one,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(20),
    );
    let seller_two_id = submit(
        &mut engine,
        seller_two,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(20),
    );

    let first_fill = engine
        .next_fill_candidate()
        .expect("first seller should cross the large buy");
    assert_eq!(first_fill.buy_id, buyer_id);
    assert_eq!(first_fill.sell_id, seller_one_id);
    engine.apply_settlement_success(&first_fill);
    assert_eq!(engine.stats.orders_matched, 2);
    assert_eq!(engine.stats.successful_settlements, 1);

    let second_fill = engine
        .next_fill_candidate()
        .expect("second seller should cross the remaining buy");
    assert_eq!(second_fill.buy_id, buyer_id);
    assert_eq!(second_fill.sell_id, seller_two_id);
    engine.apply_settlement_success(&second_fill);

    assert_eq!(engine.stats.orders_matched, 3);
    assert_eq!(engine.stats.order_sides_filled, 4);
    assert_eq!(engine.stats.successful_settlements, 2);
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.orders_with_successful_fill, 3);
    assert_eq!(snapshot.fills_settled, 2);
}

#[test]
fn rejection_stats_are_split_by_reason() {
    let mut engine = Engine::new();
    let buyer = address(1);
    engine.apply_balance_refresh(buyer, wad(5), U256::ZERO);

    let bad_request = engine.submit_order(SubmitOrderRequest {
        user: buyer,
        side: Side::Buy,
        order_type: OrderType::Limit,
        price: wad(1),
        size: U256::ZERO,
    });
    assert!(matches!(bad_request, Err(ApiError::BadRequest(_))));

    let insufficient = engine.submit_order(SubmitOrderRequest {
        user: buyer,
        side: Side::Buy,
        order_type: OrderType::Limit,
        price: wad(1),
        size: wad(10),
    });
    assert!(matches!(insufficient, Err(ApiError::BadRequest(_))));

    let stale_user = address(2);
    let stale_cache = engine.submit_order(SubmitOrderRequest {
        user: stale_user,
        side: Side::Sell,
        order_type: OrderType::Limit,
        price: wad(1),
        size: wad(1),
    });
    assert!(matches!(stale_cache, Err(ApiError::Chain(_))));

    engine.record_admission_refresh_failed();
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.orders_received, 4);
    assert_eq!(snapshot.orders_rejected, 4);
    assert_eq!(snapshot.orders_rejected_bad_request, 1);
    assert_eq!(snapshot.orders_rejected_insufficient_balance, 1);
    assert_eq!(snapshot.orders_rejected_stale_balance_cache, 1);
    assert_eq!(snapshot.orders_failed_balance_refresh, 1);
}

#[test]
fn settlement_failure_stats_are_split_by_failure_class() {
    let mut engine = Engine::new();
    engine.record_settlement_tx_attempt();
    engine.record_settlement_send_failed();
    engine.record_settlement_tx_attempt();
    engine.record_settlement_receipt_failed();
    engine.record_settlement_tx_attempt();
    engine.record_settlement_reverted();

    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.settlement_tx_attempts, 3);
    assert_eq!(snapshot.settlement_send_failures, 1);
    assert_eq!(snapshot.settlement_receipt_failures, 1);
    assert_eq!(snapshot.settlements_reverted, 1);
    assert_eq!(snapshot.settlement_receipt_reverts, 1);
    assert_eq!(snapshot.settlements_reverted_pct, 100.0 / 3.0);
}

#[test]
fn in_flight_order_is_not_selected_for_another_fill() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller_one = address(2);
    let seller_two = address(3);
    engine.apply_balance_refresh(buyer, wad(100), U256::ZERO);
    engine.apply_balance_refresh(seller_one, wad(20), U256::ZERO);
    engine.apply_balance_refresh(seller_two, wad(20), U256::ZERO);

    let buyer_id = submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(50),
    );
    submit(
        &mut engine,
        seller_one,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(20),
    );
    submit(
        &mut engine,
        seller_two,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(20),
    );

    let first_fill = engine
        .next_fill_candidate()
        .expect("first seller should cross the large buy");
    assert_eq!(first_fill.buy_id, buyer_id);

    assert!(engine.next_fill_candidate().is_none());

    engine.abort_fill(&first_fill, false, false);
    assert!(engine.next_fill_candidate().is_some());
}

#[test]
fn fill_candidates_receive_monotonic_sequence_numbers() {
    let mut engine = Engine::new();
    let buyer_one = address(1);
    let seller_one = address(2);
    let buyer_two = address(3);
    let seller_two = address(4);
    engine.apply_balance_refresh(buyer_one, wad(20), U256::ZERO);
    engine.apply_balance_refresh(seller_one, wad(20), U256::ZERO);
    engine.apply_balance_refresh(buyer_two, wad(20), U256::ZERO);
    engine.apply_balance_refresh(seller_two, wad(20), U256::ZERO);

    submit(
        &mut engine,
        buyer_one,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(10),
    );
    submit(
        &mut engine,
        seller_one,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(10),
    );
    submit(
        &mut engine,
        buyer_two,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(10),
    );
    submit(
        &mut engine,
        seller_two,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(10),
    );

    let first_fill = engine
        .next_fill_candidate()
        .expect("first pair should cross");
    let second_fill = engine
        .next_fill_candidate()
        .expect("second pair should cross");

    assert_eq!(first_fill.seq, 1);
    assert_eq!(second_fill.seq, 2);
}

#[test]
fn limit_pair_priority_prefers_better_prices() {
    let better_buy = order(Side::Buy, 11, 10);
    let worse_buy = order(Side::Buy, 10, 1);
    let sell = order(Side::Sell, 9, 1);
    assert_eq!(
        limit_pair_priority((&better_buy, &sell), (&worse_buy, &sell)),
        Ordering::Less
    );

    let buy = order(Side::Buy, 11, 1);
    let better_sell = order(Side::Sell, 9, 10);
    let worse_sell = order(Side::Sell, 10, 1);
    assert_eq!(
        limit_pair_priority((&buy, &better_sell), (&buy, &worse_sell)),
        Ordering::Less
    );
}

#[test]
fn limit_pair_priority_is_fifo_within_price_levels() {
    let older_buy = order(Side::Buy, 10, 1);
    let newer_buy = order(Side::Buy, 10, 2);
    let same_sell = order(Side::Sell, 9, 3);
    assert_eq!(
        limit_pair_priority((&newer_buy, &same_sell), (&older_buy, &same_sell)),
        Ordering::Greater
    );

    let same_buy = order(Side::Buy, 10, 1);
    let older_sell = order(Side::Sell, 9, 2);
    let newer_sell = order(Side::Sell, 9, 3);
    assert_eq!(
        limit_pair_priority((&same_buy, &older_sell), (&same_buy, &newer_sell)),
        Ordering::Less
    );
}

#[test]
fn buy_order_with_overflowing_notional_is_rejected() {
    let mut engine = Engine::new();
    let buyer = address(1);
    engine.apply_balance_refresh(buyer, U256::MAX, U256::ZERO);

    let result = engine.submit_order(SubmitOrderRequest {
        user: buyer,
        side: Side::Buy,
        order_type: OrderType::Limit,
        price: U256::MAX,
        size: U256::MAX,
    });

    assert!(matches!(result, Err(ApiError::BadRequest(_))));
    assert_eq!(engine.stats.orders_accepted, 0);
    assert_eq!(engine.stats.orders_rejected, 1);
    assert!(engine.open_orders(Some(buyer)).is_empty());
}
