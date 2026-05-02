use alloy::primitives::{Address, U256};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::env;

fn base_url() -> String {
    env::var("HARNESS_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".into())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Limit,
    Market,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Open,
    Filled,
    PartiallyFilled,
    Cancelled,
    Stale,
}

#[derive(Debug, Deserialize)]
pub struct OrderResponse {
    pub order_id: String,
    pub status: OrderStatus,
}

#[derive(Debug, Deserialize)]
pub struct BalanceView {
    pub real: U256,
    pub reserved: U256,
    pub virtual_: U256,
    pub vault: U256,
}

#[derive(Debug, Deserialize)]
pub struct Order {
    pub id: String,
    pub user: Address,
    pub side: Side,
    pub order_type: OrderType,
    pub price: U256,
    pub size: U256,
    pub filled_size: U256,
    pub status: OrderStatus,
}

#[derive(Serialize)]
struct SubmitOrderRequest {
    user: Address,
    side: Side,
    order_type: OrderType,
    price: U256,
    size: U256,
}

pub async fn submit_order(
    client: &reqwest::Client,
    user: Address,
    side: Side,
    order_type: OrderType,
    price: U256,
    size: U256,
) -> Result<OrderResponse> {
    let resp = client
        .post(format!("{}/orders", base_url()))
        .json(&SubmitOrderRequest { user, side, order_type, price, size })
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn cancel_order(client: &reqwest::Client, order_id: &str) -> Result<()> {
    client
        .delete(format!("{}/orders/{}", base_url(), order_id))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

pub async fn get_user_balance_view(
    client: &reqwest::Client,
    address: Address,
) -> Result<BalanceView> {
    let resp = client
        .get(format!("{}/balances/{}", base_url(), address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

pub async fn get_open_orders(
    client: &reqwest::Client,
    address: Address,
) -> Result<Vec<Order>> {
    let resp = client
        .get(format!("{}/orders?user={}", base_url(), address))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}
