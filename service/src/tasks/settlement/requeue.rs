use super::SettlementConfig;
use crate::AppState;

pub(super) async fn claim_and_enqueue_available_fills(state: &AppState) {
    let max = SettlementConfig::from_env().max_fill_claim_batch;
    claim_and_enqueue_available_fills_with_limit(state, max).await;
}

pub(super) async fn claim_and_enqueue_available_fills_with_limit(state: &AppState, max: usize) {
    let fills = {
        let mut engine = state.engine.lock().await;
        engine.claim_fill_batch(max.max(1))
    };

    if fills.is_empty() {
        return;
    }

    for fill in fills.iter().cloned() {
        if state.settlement_queue.send(fill).is_err() {
            let mut engine = state.engine.lock().await;
            for claimed in &fills {
                if engine.fill_still_pending(claimed) {
                    engine.record_settlement_aborted_before_tx();
                    engine.abort_fill(claimed, false, false);
                }
            }
            return;
        }
    }
}
