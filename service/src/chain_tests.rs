use super::*;
use std::collections::HashMap;

fn address(byte: u8) -> Address {
    Address::from([byte; 20])
}

fn topic_for(user: Address) -> String {
    let user = format!("{:#x}", user);
    let user = user.strip_prefix("0x").unwrap_or(&user);
    format!("0x{user:0>64}")
}

#[test]
fn indexed_address_topic_parses_last_twenty_bytes() {
    let user = address(7);

    assert_eq!(address_from_topic(&topic_for(user)), Some(user));
    assert_eq!(address_from_topic("0x1234"), None);
}

#[test]
fn dirty_log_collection_keeps_latest_block_per_user() {
    let user = address(1);
    let other = address(2);
    let log = RpcLog {
        topics: vec![
            event_topic("Transfer(address,address,uint256)"),
            topic_for(user),
            topic_for(other),
        ],
        block_number: Some("0xa".into()),
    };
    let mut users = HashMap::<Address, u64>::new();

    collect_indexed_address_at_block(&log, 1, 10, &mut users);
    collect_indexed_address_at_block(&log, 1, 8, &mut users);
    collect_indexed_address_at_block(&log, 2, 12, &mut users);

    assert_eq!(users.get(&user), Some(&10));
    assert_eq!(users.get(&other), Some(&12));
}
