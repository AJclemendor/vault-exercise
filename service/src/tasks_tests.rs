use super::*;

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
    assert_eq!(snapshot.settlement_receipt_reverts, 1);
    assert_eq!(snapshot.settlement_receipt_failures, 1);
    assert_eq!(snapshot.settlement_send_failures, 0);
}
