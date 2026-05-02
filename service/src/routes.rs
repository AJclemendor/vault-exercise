use crate::AppState;
use crate::stats::StatsSnapshot;
use crate::types::{
    ApiError, BalanceView, BookQuery, BookSnapshot, OrderResponse, OrderView, OrdersQuery,
    SubmitOrderRequest,
};
use alloy::primitives::Address;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use std::str::FromStr;

pub(crate) async fn submit_order(
    State(state): State<AppState>,
    Json(request): Json<SubmitOrderRequest>,
) -> std::result::Result<Json<OrderResponse>, ApiError> {
    let ticket = state.admission.issue_ticket();
    let user = request.user;
    let needs_refresh = {
        let engine = state.engine.lock().await;
        engine.balance_needs_admission_refresh(user)
    };

    let refresh_result = if needs_refresh {
        Some(state.chain.read_user_balances(user).await)
    } else {
        None
    };

    let _turn = state.admission.wait_for_turn(ticket).await;

    if let Some(refresh_result) = refresh_result {
        match refresh_result {
            Ok((real, vault)) => {
                let mut engine = state.engine.lock().await;
                engine.apply_balance_refresh(user, real, vault);
                engine.record_admission_refresh_succeeded();
            }
            Err(err) => {
                let mut engine = state.engine.lock().await;
                engine.record_admission_refresh_failed();
                return Err(ApiError::Chain(format!(
                    "failed to refresh balance for admission: {err:#}"
                )));
            }
        }
    }

    let admission = {
        let mut engine = state.engine.lock().await;
        engine.submit_order_and_claim_fills(request)?
    };
    for fill in admission.fills {
        if state.settlement_queue.send(fill).is_err() {
            return Err(ApiError::Chain("settlement queue is closed".into()));
        }
    }
    Ok(Json(admission.response))
}

pub(crate) async fn cancel_order(
    State(state): State<AppState>,
    Path(order_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let mut engine = state.engine.lock().await;
    engine.cancel_order(&order_id)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn get_balance(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> std::result::Result<Json<BalanceView>, ApiError> {
    let user = Address::from_str(&address)
        .map_err(|_| ApiError::BadRequest(format!("invalid address {address}")))?;
    let needs_refresh = {
        let engine = state.engine.lock().await;
        engine.balance_needs_admission_refresh(user)
    };

    if needs_refresh {
        let (real, vault) =
            state.chain.read_user_balances(user).await.map_err(|err| {
                ApiError::Chain(format!("failed to refresh balance view: {err:#}"))
            })?;
        let mut engine = state.engine.lock().await;
        engine.apply_balance_refresh(user, real, vault);
        engine.prune_user_to_balance(user, None);
    }

    let engine = state.engine.lock().await;
    Ok(Json(engine.balance_view(user)))
}

pub(crate) async fn list_orders(
    State(state): State<AppState>,
    Query(query): Query<OrdersQuery>,
) -> Json<Vec<OrderView>> {
    let engine = state.engine.lock().await;
    Json(engine.open_orders(query.user))
}

pub(crate) async fn get_book(
    State(state): State<AppState>,
    Query(query): Query<BookQuery>,
) -> Json<BookSnapshot> {
    let engine = state.engine.lock().await;
    Json(engine.book_snapshot(query.depth.unwrap_or(10)))
}

pub(crate) async fn get_stats(State(state): State<AppState>) -> Json<StatsSnapshot> {
    let engine = state.engine.lock().await;
    Json(engine.stats_snapshot())
}
