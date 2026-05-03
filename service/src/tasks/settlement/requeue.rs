use crate::AppState;

const REQUEUE_CLAIM_BATCH: usize = 16;

pub(super) async fn claim_and_enqueue_available_fills(state: &AppState) {
    let fills = {
        let mut engine = state.engine.lock().await;
        engine.claim_fill_batch(REQUEUE_CLAIM_BATCH)
    };

    if fills.is_empty() {
        return;
    }

    for (index, fill) in fills.iter().cloned().enumerate() {
        if state.settlement_queue.send(fill).is_err() {
            let mut engine = state.engine.lock().await;
            for unsent in &fills[index..] {
                engine.abort_fill(unsent, false, false);
            }
            return;
        }
    }
}
