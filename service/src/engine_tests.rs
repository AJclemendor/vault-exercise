use super::*;

fn address(byte: u8) -> Address {
    Address::from([byte; 20])
}

fn wad(value: u64) -> U256 {
    U256::from(value) * U256::from(WAD)
}

fn tenth_wad(value: u64) -> U256 {
    U256::from(value) * U256::from(WAD) / U256::from(10u8)
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

fn insert_indexed(engine: &mut Engine, order: Order) {
    let id = order.id.clone();
    if order.order_type == OrderType::Limit {
        engine.index_limit_order(order.side, order.price, id.clone());
    }
    engine.orders.insert(id, order);
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
    let view = engine.balance_view(user);
    assert!(view.stale);
    assert!(view.last_refresh_age_ms.is_some());
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
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.market_ioc_orders_accepted, 1);
    assert_eq!(snapshot.currently_open_market_ioc_orders, 0);
    assert_eq!(snapshot.market_ioc_orders_cancelled_unfilled, 1);
}

#[test]
fn market_order_matches_older_resting_limit_from_indexed_book() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller = address(2);
    engine.apply_balance_refresh(buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);

    let sell_id = submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        tenth_wad(8),
        wad(10),
    );
    let market_id = submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Market,
        wad(1),
        wad(10),
    );

    let fill = engine
        .next_fill_candidate()
        .expect("market buy should cross older resting ask");

    assert_eq!(fill.buy_id, market_id);
    assert_eq!(fill.sell_id, sell_id);
    assert_eq!(fill.quote, wad(8));
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
fn precheck_allows_price_improved_fill_when_actual_debit_is_affordable() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller = address(2);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);
    engine.apply_balance_refresh(buyer, wad(20), U256::ZERO);

    let sell_id = submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(10),
    );
    let buy_id = submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(2),
        wad(10),
    );

    let fill = engine
        .next_fill_candidate()
        .expect("newer bid should cross the resting ask");
    assert_eq!(fill.buy_id, buy_id);
    assert_eq!(fill.sell_id, sell_id);
    assert_eq!(fill.quote, wad(10));
    assert_eq!(engine.balance_view(buyer).reserved, wad(20));

    engine.apply_balance_refresh(buyer, wad(10), U256::ZERO);
    let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(&fill);

    assert!(buyer_ok);
    assert!(seller_ok);
    assert!(engine.fill_still_pending(&fill));
    assert_eq!(engine.orders[&buy_id].status, OrderStatus::Open);
    assert_eq!(engine.orders[&sell_id].status, OrderStatus::Open);
}

#[test]
fn precheck_allows_price_improved_fill_when_actual_debit_is_funded() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller = address(2);
    engine.apply_balance_refresh(buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);

    let sell_id = submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        tenth_wad(8),
        wad(10),
    );
    let buy_id = submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(10),
    );

    let fill = engine
        .next_fill_candidate()
        .expect("price-improved orders should cross");
    assert_eq!(fill.buy_id, buy_id);
    assert_eq!(fill.sell_id, sell_id);
    assert_eq!(fill.quote, wad(8));

    engine.apply_balance_refresh(buyer, wad(8), U256::ZERO);
    let (buyer_ok, seller_ok) = engine.prune_underfunded_fill_users(&fill);

    assert!(buyer_ok);
    assert!(seller_ok);
    assert!(engine.fill_still_pending(&fill));
    assert_eq!(engine.orders[&buy_id].status, OrderStatus::Open);
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
    assert_eq!(engine.stats.unique_orders_with_successful_fill, 0);

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
    assert_eq!(engine.stats.unique_orders_with_successful_fill, 2);
    assert_eq!(engine.stats.order_sides_filled, 2);
    assert_eq!(engine.stats.successful_settlements, 1);
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.orders_matched, 2);
    assert_eq!(snapshot.orders_with_successful_fill, 2);
    assert_eq!(snapshot.unique_orders_filled, 2);
    assert_eq!(snapshot.unique_orders_with_successful_fill, 2);
    assert_eq!(snapshot.order_sides_filled, 2);
    assert_eq!(snapshot.order_fill_side_events, 2);
    assert_eq!(snapshot.fill_sides_successfully_settled, 2);
    assert_eq!(snapshot.fills_settled, 1);
    assert_eq!(snapshot.fills_successfully_settled, 1);
}

#[test]
fn settlement_snapshot_does_not_exceed_candidate_denominator_before_precheck() {
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

    let _fill = engine.next_fill_candidate().expect("orders should cross");
    let snapshot = engine.stats_snapshot();

    assert_eq!(snapshot.fill_candidates, 1);
    assert_eq!(snapshot.settlements_attempted, 1);
    assert_eq!(snapshot.fill_candidates_pct_of_settlements_attempted, 100.0);
    assert_eq!(snapshot.settlement_precheck_attempts, 0);
    assert_eq!(snapshot.settlement_precheck_attempts_pct_of_candidates, 0.0);
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
    assert_eq!(engine.stats.unique_orders_with_successful_fill, 2);
    assert_eq!(engine.stats.successful_settlements, 1);

    let second_fill = engine
        .next_fill_candidate()
        .expect("second seller should cross the remaining buy");
    assert_eq!(second_fill.buy_id, buyer_id);
    assert_eq!(second_fill.sell_id, seller_two_id);
    engine.apply_settlement_success(&second_fill);

    assert_eq!(engine.stats.unique_orders_with_successful_fill, 3);
    assert_eq!(engine.stats.order_sides_filled, 4);
    assert_eq!(engine.stats.successful_settlements, 2);
    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.orders_matched, 3);
    assert_eq!(snapshot.orders_with_successful_fill, 3);
    assert_eq!(snapshot.unique_orders_filled, 3);
    assert_eq!(snapshot.unique_orders_with_successful_fill, 3);
    assert_eq!(snapshot.order_sides_filled, 4);
    assert_eq!(snapshot.order_fill_side_events, 4);
    assert_eq!(snapshot.fill_sides_successfully_settled, 4);
    assert_eq!(snapshot.fills_settled, 2);
    assert_eq!(snapshot.fills_successfully_settled, 2);
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
    assert_eq!(snapshot.orders_admission_failures, 4);
    assert_eq!(snapshot.orders_admission_failures_pct, 100.0);
}

#[test]
fn balance_view_exposes_over_reserved_deficit() {
    let mut engine = Engine::new();
    let seller = address(1);
    engine.apply_balance_refresh(seller, wad(20), U256::ZERO);

    submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(10),
    );
    engine.apply_balance_refresh(seller, wad(5), U256::ZERO);

    let balance = engine.balance_view(seller);
    assert_eq!(balance.real, wad(5));
    assert_eq!(balance.reserved, wad(10));
    assert_eq!(balance.virtual_, U256::ZERO);
    assert_eq!(balance.deficit, wad(5));
    assert!(balance.over_reserved);
}

#[test]
fn open_orders_are_returned_in_created_sequence_order() {
    let mut engine = Engine::new();
    let seller = address(1);
    engine.apply_balance_refresh(seller, wad(20), U256::ZERO);

    for _ in 0..12 {
        submit(
            &mut engine,
            seller,
            Side::Sell,
            OrderType::Limit,
            wad(1),
            wad(1),
        );
    }

    let ids: Vec<_> = engine
        .open_orders(Some(seller))
        .into_iter()
        .map(|order| order.id)
        .collect();
    assert_eq!(ids[1], "ord-2");
    assert_eq!(ids[9], "ord-10");
    assert_eq!(ids[11], "ord-12");
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
    engine.record_settlement_unknown_outcome();

    let snapshot = engine.stats_snapshot();
    assert_eq!(snapshot.settlement_tx_attempts, 3);
    assert_eq!(snapshot.settlement_precheck_attempts, 0);
    assert_eq!(snapshot.settlement_precheck_passed, 0);
    assert_eq!(snapshot.settlement_tx_submitted, 2);
    assert_eq!(snapshot.settlement_send_failures, 1);
    assert_eq!(snapshot.settlement_receipt_failures, 1);
    assert_eq!(snapshot.settlements_reverted, 1);
    assert_eq!(snapshot.settlement_reverts, 1);
    assert_eq!(snapshot.settlement_receipt_status_reverted, 1);
    assert_eq!(snapshot.settlement_tx_reverts, 1);
    assert_eq!(snapshot.settlements_reverted_pct, 100.0 / 3.0);
    assert_eq!(snapshot.settlement_tx_failures, 3);
    assert_eq!(snapshot.settlement_tx_failures_pct, 100.0);
    assert_eq!(snapshot.settlement_failures, 3);
    assert_eq!(snapshot.settlement_terminal_outcomes, 3);
    assert_eq!(snapshot.settlement_pending_outcomes, 0);
    assert_eq!(snapshot.settlement_unknown_outcomes, 1);
}

#[test]
fn crossing_limit_search_skips_self_match_price_levels() {
    let mut engine = Engine::new();
    let shared_user = address(1);
    let other_seller = address(2);
    engine.apply_balance_refresh(shared_user, wad(100), U256::ZERO);
    engine.apply_balance_refresh(other_seller, wad(10), U256::ZERO);

    let buy_id = submit(
        &mut engine,
        shared_user,
        Side::Buy,
        OrderType::Limit,
        wad(10),
        wad(1),
    );
    let self_sell_id = submit(
        &mut engine,
        shared_user,
        Side::Sell,
        OrderType::Limit,
        wad(5),
        wad(1),
    );
    let other_sell_id = submit(
        &mut engine,
        other_seller,
        Side::Sell,
        OrderType::Limit,
        wad(6),
        wad(1),
    );

    let fill = engine
        .next_fill_candidate()
        .expect("compatible later ask level should be selected");

    assert_eq!(fill.buy_id, buy_id);
    assert_eq!(fill.sell_id, other_sell_id);
    assert_eq!(engine.orders[&self_sell_id].status, OrderStatus::Open);
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
fn best_crossing_limits_preserves_price_priority_before_fifo_when_skipping_self_trade() {
    let mut engine = Engine::new();
    let user_a = address(1);
    let user_b = address(2);

    let mut older_buy = order(Side::Buy, 10, 1);
    older_buy.user = user_a;
    let mut better_sell_same_user = order(Side::Sell, 8, 2);
    better_sell_same_user.user = user_a;
    let mut worse_sell_other_user = order(Side::Sell, 9, 3);
    worse_sell_other_user.user = user_b;
    let mut newer_buy_other_user = order(Side::Buy, 10, 4);
    newer_buy_other_user.user = user_b;

    insert_indexed(&mut engine, older_buy);
    insert_indexed(&mut engine, better_sell_same_user);
    insert_indexed(&mut engine, worse_sell_other_user);
    insert_indexed(&mut engine, newer_buy_other_user);

    let (buy_id, sell_id) = engine
        .best_crossing_limits()
        .expect("there should be a non-self crossing pair");

    assert_eq!(buy_id, "ord-4");
    assert_eq!(sell_id, "ord-2");
}

#[test]
fn best_bid_wins_before_older_worse_bid() {
    let mut engine = Engine::new();
    let older_buyer = address(1);
    let better_buyer = address(2);
    let seller = address(3);
    engine.apply_balance_refresh(older_buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(better_buyer, wad(20), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);

    submit(
        &mut engine,
        older_buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    let better_buy_id = submit(
        &mut engine,
        better_buyer,
        Side::Buy,
        OrderType::Limit,
        wad(2),
        wad(1),
    );
    let sell_id = submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );

    let fill = engine
        .next_fill_candidate()
        .expect("best bid should cross first");
    assert_eq!(fill.buy_id, better_buy_id);
    assert_eq!(fill.sell_id, sell_id);
}

#[test]
fn best_ask_wins_before_older_worse_ask() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let older_seller = address(2);
    let better_seller = address(3);
    engine.apply_balance_refresh(buyer, wad(20), U256::ZERO);
    engine.apply_balance_refresh(older_seller, wad(20), U256::ZERO);
    engine.apply_balance_refresh(better_seller, wad(20), U256::ZERO);

    submit(
        &mut engine,
        older_seller,
        Side::Sell,
        OrderType::Limit,
        wad(2),
        wad(1),
    );
    let better_sell_id = submit(
        &mut engine,
        better_seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    let buy_id = submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(2),
        wad(1),
    );

    let fill = engine
        .next_fill_candidate()
        .expect("best ask should cross first");
    assert_eq!(fill.buy_id, buy_id);
    assert_eq!(fill.sell_id, better_sell_id);
}

#[test]
fn fifo_holds_within_same_bid_price() {
    let mut engine = Engine::new();
    let older_buyer = address(1);
    let newer_buyer = address(2);
    let seller = address(3);
    engine.apply_balance_refresh(older_buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(newer_buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);

    let older_buy_id = submit(
        &mut engine,
        older_buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    submit(
        &mut engine,
        newer_buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );

    let fill = engine
        .next_fill_candidate()
        .expect("oldest same-price bid should cross first");
    assert_eq!(fill.buy_id, older_buy_id);
}

#[test]
fn fifo_holds_within_same_ask_price() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let older_seller = address(2);
    let newer_seller = address(3);
    engine.apply_balance_refresh(buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(older_seller, wad(10), U256::ZERO);
    engine.apply_balance_refresh(newer_seller, wad(10), U256::ZERO);

    let older_sell_id = submit(
        &mut engine,
        older_seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    submit(
        &mut engine,
        newer_seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );

    let fill = engine
        .next_fill_candidate()
        .expect("oldest same-price ask should cross first");
    assert_eq!(fill.sell_id, older_sell_id);
}

#[test]
fn oldest_market_order_is_handled_first() {
    let mut engine = Engine::new();
    let seller = address(1);
    let first_buyer = address(2);
    let second_buyer = address(3);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);
    engine.apply_balance_refresh(first_buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(second_buyer, wad(10), U256::ZERO);

    let sell_id = submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(2),
    );
    let first_market_id = submit(
        &mut engine,
        first_buyer,
        Side::Buy,
        OrderType::Market,
        wad(1),
        wad(1),
    );
    submit(
        &mut engine,
        second_buyer,
        Side::Buy,
        OrderType::Market,
        wad(1),
        wad(1),
    );

    let fill = engine
        .next_fill_candidate()
        .expect("oldest market order should be handled first");
    assert_eq!(fill.buy_id, first_market_id);
    assert_eq!(fill.sell_id, sell_id);
}

#[test]
fn claimed_fill_marks_both_orders_in_flight() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller = address(2);
    engine.apply_balance_refresh(buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);

    let buy_id = submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    let sell_id = submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );

    let fill = engine.next_fill_candidate().expect("orders should cross");

    assert_eq!(engine.orders[&buy_id].in_flight_size, fill.fill_size);
    assert_eq!(engine.orders[&sell_id].in_flight_size, fill.fill_size);
}

#[test]
fn abort_releases_in_flight_and_preserves_priority() {
    let mut engine = Engine::new();
    let older_buyer = address(1);
    let newer_buyer = address(2);
    let seller = address(3);
    engine.apply_balance_refresh(older_buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(newer_buyer, wad(10), U256::ZERO);
    engine.apply_balance_refresh(seller, wad(10), U256::ZERO);

    let older_buy_id = submit(
        &mut engine,
        older_buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    submit(
        &mut engine,
        newer_buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    let sell_id = submit(
        &mut engine,
        seller,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );

    let aborted = engine
        .next_fill_candidate()
        .expect("oldest bid should cross first");
    engine.abort_fill(&aborted, false, false);
    let retried = engine
        .next_fill_candidate()
        .expect("same priority fill should be selected after abort");

    assert_eq!(retried.buy_id, older_buy_id);
    assert_eq!(retried.sell_id, sell_id);
}

#[test]
fn partial_success_updates_indexed_book_levels() {
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
        wad(20),
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
    engine.apply_settlement_success(&fill);

    let snapshot = engine.book_snapshot(10);
    assert_eq!(snapshot.bids.len(), 1);
    assert_eq!(snapshot.bids[0].price_raw, wad(1));
    assert_eq!(snapshot.bids[0].size_raw, wad(10));
    assert!(snapshot.asks.is_empty());
}

#[test]
fn claim_fill_batch_matches_repeated_single_claims() {
    let mut batched = Engine::new();
    let mut repeated = Engine::new();
    for engine in [&mut batched, &mut repeated] {
        for byte in 1..=4 {
            engine.apply_balance_refresh(address(byte), wad(10), U256::ZERO);
        }
        submit(
            engine,
            address(1),
            Side::Buy,
            OrderType::Limit,
            wad(1),
            wad(1),
        );
        submit(
            engine,
            address(2),
            Side::Sell,
            OrderType::Limit,
            wad(1),
            wad(1),
        );
        submit(
            engine,
            address(3),
            Side::Buy,
            OrderType::Limit,
            wad(1),
            wad(1),
        );
        submit(
            engine,
            address(4),
            Side::Sell,
            OrderType::Limit,
            wad(1),
            wad(1),
        );
    }

    let batched_pairs: Vec<_> = batched
        .claim_fill_batch(2)
        .into_iter()
        .map(|fill| (fill.seq, fill.buy_id, fill.sell_id))
        .collect();
    let repeated_pairs: Vec<_> = (0..2)
        .map(|_| {
            let fill = repeated
                .claim_next_fill_candidate()
                .expect("fill should be available");
            (fill.seq, fill.buy_id, fill.sell_id)
        })
        .collect();

    assert_eq!(batched_pairs, repeated_pairs);
}

#[test]
fn claim_fill_batch_does_not_double_select_in_flight_order() {
    let mut engine = Engine::new();
    let buyer = address(1);
    let seller_one = address(2);
    let seller_two = address(3);
    engine.apply_balance_refresh(buyer, wad(20), U256::ZERO);
    engine.apply_balance_refresh(seller_one, wad(10), U256::ZERO);
    engine.apply_balance_refresh(seller_two, wad(10), U256::ZERO);

    submit(
        &mut engine,
        buyer,
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(20),
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
        seller_two,
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(10),
    );

    let fills = engine.claim_fill_batch(2);

    assert_eq!(fills.len(), 1);
}

#[test]
fn claim_fill_batch_preserves_fifo_within_price_level() {
    let mut engine = Engine::new();
    for byte in 1..=6 {
        engine.apply_balance_refresh(address(byte), wad(10), U256::ZERO);
    }

    let first_buy = submit(
        &mut engine,
        address(1),
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    let second_buy = submit(
        &mut engine,
        address(2),
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    let first_sell = submit(
        &mut engine,
        address(3),
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    let second_sell = submit(
        &mut engine,
        address(4),
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );

    let fills = engine.claim_fill_batch(2);

    assert_eq!(fills[0].buy_id, first_buy);
    assert_eq!(fills[0].sell_id, first_sell);
    assert_eq!(fills[1].buy_id, second_buy);
    assert_eq!(fills[1].sell_id, second_sell);
}

#[test]
fn claim_multiple_fills_do_not_overlap_orders() {
    let mut engine = Engine::new();
    for byte in 1..=4 {
        engine.apply_balance_refresh(address(byte), wad(10), U256::ZERO);
    }

    submit(
        &mut engine,
        address(1),
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    submit(
        &mut engine,
        address(2),
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    submit(
        &mut engine,
        address(3),
        Side::Buy,
        OrderType::Limit,
        wad(1),
        wad(1),
    );
    submit(
        &mut engine,
        address(4),
        Side::Sell,
        OrderType::Limit,
        wad(1),
        wad(1),
    );

    let fills = engine.claim_fill_batch(2);

    assert_ne!(fills[0].buy_id, fills[1].buy_id);
    assert_ne!(fills[0].sell_id, fills[1].sell_id);
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
