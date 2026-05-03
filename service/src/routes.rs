use crate::AppState;
use crate::stats::StatsSnapshot;
use crate::types::{
    ApiError, BalanceView, BookQuery, BookSnapshot, OrderResponse, OrderView, OrdersQuery,
    SubmitOrderRequest,
};
use alloy::primitives::Address;
use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use std::str::FromStr;

pub(crate) async fn submit_order(
    State(state): State<AppState>,
    request: std::result::Result<Json<SubmitOrderRequest>, JsonRejection>,
) -> std::result::Result<Json<OrderResponse>, ApiError> {
    let Json(request) = request.map_err(json_error)?;
    let ticket = state.admission.issue_ticket();
    let user = request.user;

    let _turn = ticket.wait_for_turn().await;

    let needs_refresh = {
        let engine = state.engine.lock().await;
        engine.balance_needs_admission_refresh(user)
    };
    if needs_refresh {
        match state.chain.read_user_balances(user).await {
            Ok(balance) => {
                let mut engine = state.engine.lock().await;
                engine.apply_balance_refresh_at_block(
                    user,
                    balance.real,
                    balance.vault,
                    balance.block,
                );
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
    for (index, fill) in admission.fills.iter().cloned().enumerate() {
        if state.settlement_queue.send(fill).is_err() {
            let mut engine = state.engine.lock().await;
            for unsent in &admission.fills[index..] {
                engine.abort_fill(unsent, false, false);
            }
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
    let balance = state
        .chain
        .read_user_balances(user)
        .await
        .map_err(|err| ApiError::Chain(format!("failed to read balance view: {err:#}")))?;
    let engine = state.engine.lock().await;
    Ok(Json(engine.balance_view_with_chain_values(
        user,
        balance.real,
        balance.vault,
    )))
}

pub(crate) async fn list_orders(
    State(state): State<AppState>,
    query: std::result::Result<Query<OrdersQuery>, QueryRejection>,
) -> std::result::Result<Json<Vec<OrderView>>, ApiError> {
    let Query(query) = query.map_err(query_error)?;
    let engine = state.engine.lock().await;
    Ok(Json(engine.open_orders(query.user)))
}

pub(crate) async fn get_book(
    State(state): State<AppState>,
    query: std::result::Result<Query<BookQuery>, QueryRejection>,
) -> std::result::Result<Json<BookSnapshot>, ApiError> {
    let Query(query) = query.map_err(query_error)?;
    let depth = validated_depth(query)?;
    let engine = state.engine.lock().await;
    Ok(Json(engine.book_snapshot(depth)))
}

pub(crate) async fn get_stats(State(state): State<AppState>) -> Json<StatsSnapshot> {
    let engine = state.engine.lock().await;
    Json(engine.stats_snapshot())
}

fn validated_depth(query: BookQuery) -> std::result::Result<usize, ApiError> {
    let depth = query.depth.unwrap_or(10);
    if !(1..=100).contains(&depth) {
        return Err(ApiError::BadRequest(
            "book depth must be between 1 and 100".into(),
        ));
    }
    Ok(depth)
}

fn json_error(err: JsonRejection) -> ApiError {
    ApiError::BadRequest(format!("invalid JSON request: {err}"))
}

fn query_error(err: QueryRejection) -> ApiError {
    ApiError::BadRequest(format!("invalid query string: {err}"))
}
