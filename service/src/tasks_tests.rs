use super::*;
use tokio::time::timeout;

#[tokio::test]
async fn settlement_sequencer_advances_through_out_of_order_completions() {
    let sequencer = Arc::new(SettlementSequencer::new());

    timeout(Duration::from_millis(10), sequencer.wait_for_turn(1))
        .await
        .expect("first sequence should be immediately available");

    let waiting_for_two = {
        let sequencer = Arc::clone(&sequencer);
        tokio::spawn(async move {
            sequencer.wait_for_turn(2).await;
        })
    };

    tokio::task::yield_now().await;
    assert!(!waiting_for_two.is_finished());

    sequencer.complete(3).await;
    tokio::task::yield_now().await;
    assert!(!waiting_for_two.is_finished());

    sequencer.complete(1).await;
    timeout(Duration::from_millis(100), waiting_for_two)
        .await
        .expect("second sequence should unblock after first completes")
        .expect("wait task should not panic");

    sequencer.complete(2).await;
    timeout(Duration::from_millis(10), sequencer.wait_for_turn(4))
        .await
        .expect("third completion should be replayed once second completes");
}
