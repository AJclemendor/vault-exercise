use alloy::primitives::{Address, U256};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OrderType {
    Limit,
    Market,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OrderStatus {
    Open,
    Filled,
    PartiallyFilled,
    Cancelled,
    Stale,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SubmitOrderRequest {
    pub(crate) user: Address,
    pub(crate) side: Side,
    pub(crate) order_type: OrderType,
    pub(crate) price: U256,
    pub(crate) size: U256,
}

#[derive(Debug, Serialize)]
pub(crate) struct OrderResponse {
    pub(crate) order_id: String,
    pub(crate) status: OrderStatus,
}

#[derive(Debug, Serialize)]
pub(crate) struct BalanceView {
    pub(crate) real: U256,
    pub(crate) reserved: U256,
    #[serde(rename = "virtual")]
    pub(crate) virtual_: U256,
    pub(crate) deficit: U256,
    pub(crate) over_reserved: bool,
    pub(crate) vault: U256,
    pub(crate) stale: bool,
    pub(crate) last_refresh_age_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OrderView {
    pub(crate) id: String,
    pub(crate) user: Address,
    pub(crate) side: Side,
    pub(crate) order_type: OrderType,
    pub(crate) price: U256,
    pub(crate) size: U256,
    pub(crate) filled_size: U256,
    pub(crate) status: OrderStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OrdersQuery {
    pub(crate) user: Option<Address>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BookQuery {
    pub(crate) depth: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BookLevel {
    pub(crate) price: String,
    pub(crate) price_raw: U256,
    pub(crate) size: String,
    pub(crate) size_raw: U256,
    pub(crate) orders: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct BookSnapshot {
    pub(crate) depth: usize,
    pub(crate) bids: Vec<BookLevel>,
    pub(crate) asks: Vec<BookLevel>,
    pub(crate) best_bid: Option<String>,
    pub(crate) best_bid_raw: Option<U256>,
    pub(crate) best_ask: Option<String>,
    pub(crate) best_ask_raw: Option<U256>,
    pub(crate) spread: Option<String>,
    pub(crate) spread_raw: Option<U256>,
    pub(crate) mid: Option<String>,
    pub(crate) mid_raw: Option<U256>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

#[derive(Debug, Error)]
pub(crate) enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Chain(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Chain(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        let body = Json(ErrorBody {
            error: self.to_string(),
        });
        (status, body).into_response()
    }
}
