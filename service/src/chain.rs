use alloy::network::Ethereum;
use alloy::primitives::{Address, TxHash, U256, keccak256};
use alloy::providers::{PendingTransactionBuilder, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::transports::http::reqwest::Url as AlloyUrl;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;
use thiserror::Error;

use crate::runtime::chain_tuning;

sol! {
    #[sol(rpc)]
    contract MockToken {
        function balanceOf(address owner) external view returns (uint256);
    }

    #[sol(rpc)]
    contract Vault {
        function matchOrders(address a, address b, uint256 amountA, uint256 amountB) external;
        function balanceOf(address user) external view returns (uint256);
    }
}

#[derive(Clone)]
pub(crate) struct ChainClient {
    rpc_url: AlloyUrl,
    token_address: Address,
    vault_address: Address,
    operator: PrivateKeySigner,
    http: reqwest::Client,
    transfer_topic: String,
    match_topic: String,
    withdraw_topic: String,
}

impl ChainClient {
    pub(crate) fn new(
        rpc_url: String,
        token_address: String,
        vault_address: String,
        operator_key: String,
    ) -> Result<Self> {
        let rpc_url: AlloyUrl = rpc_url.parse().context("invalid rpc url")?;
        let token_address = Address::from_str(&token_address)
            .with_context(|| format!("invalid token address {token_address}"))?;
        let vault_address = Address::from_str(&vault_address)
            .with_context(|| format!("invalid vault address {vault_address}"))?;
        let operator = operator_key
            .parse::<PrivateKeySigner>()
            .context("invalid operator private key")?;
        let tuning = chain_tuning();
        let http = reqwest::Client::builder()
            .timeout(tuning.rpc_http_timeout)
            .build()
            .context("failed to build rpc http client")?;

        Ok(Self {
            rpc_url,
            token_address,
            vault_address,
            operator,
            http,
            transfer_topic: event_topic("Transfer(address,address,uint256)"),
            match_topic: event_topic("Match(address,address,uint256,uint256)"),
            withdraw_topic: event_topic("Withdraw(address,uint256)"),
        })
    }

    pub(crate) async fn read_user_balances(&self, user: Address) -> Result<BalanceRead> {
        let block = self.block_number().await?;
        let block_tag = hex_block(block);
        let data = balance_of_call_data(user);
        let responses = self
            .rpc_batch(&[
                JsonRpcRequest {
                    jsonrpc: "2.0",
                    id: 1,
                    method: "eth_call",
                    params: json!([
                        {
                            "to": format!("{:#x}", self.token_address),
                            "data": data
                        },
                        block_tag.clone()
                    ]),
                },
                JsonRpcRequest {
                    jsonrpc: "2.0",
                    id: 2,
                    method: "eth_call",
                    params: json!([
                        {
                            "to": format!("{:#x}", self.vault_address),
                            "data": data
                        },
                        block_tag.clone()
                    ]),
                },
            ])
            .await?;
        let real = responses
            .get(&1)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("token balanceOf response was not a hex string"))
            .and_then(parse_hex_u256)
            .context("token balanceOf failed")?;
        let vault_balance = responses
            .get(&2)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("vault balanceOf response was not a hex string"))
            .and_then(parse_hex_u256)
            .context("vault balanceOf failed")?;
        Ok(BalanceRead {
            real,
            vault: vault_balance,
            block,
        })
    }

    pub(crate) async fn submit_settlement(
        &self,
        buyer: Address,
        seller: Address,
        quote: U256,
        base: U256,
    ) -> Result<PendingTransactionBuilder<Ethereum>> {
        let provider = ProviderBuilder::new()
            .wallet(self.operator.clone())
            .connect_reqwest(self.http.clone(), self.rpc_url.clone());
        let vault = Vault::new(self.vault_address, &provider);
        vault
            .matchOrders(buyer, seller, quote, base)
            .send()
            .await
            .context("matchOrders send failed")
    }

    pub(crate) async fn confirm_settlement(
        &self,
        pending: PendingTransactionBuilder<Ethereum>,
    ) -> std::result::Result<(), SettlementConfirmationError> {
        let tx_hash = *pending.tx_hash();
        let (provider, config) = pending.split();
        let mut last_error = None;
        let tuning = chain_tuning();

        for attempt in 1..=tuning.receipt_confirm_retries {
            let pending = PendingTransactionBuilder::from_config(provider.clone(), config.clone());
            match pending.get_receipt().await {
                Ok(receipt) => {
                    if !receipt.status() {
                        return Err(SettlementConfirmationError::Reverted);
                    }
                    return Ok(());
                }
                Err(err) => {
                    last_error = Some(anyhow!(err).context(format!(
                        "matchOrders receipt failed for tx {tx_hash} attempt {attempt}/{}",
                        tuning.receipt_confirm_retries
                    )));
                    if attempt < tuning.receipt_confirm_retries {
                        tokio::time::sleep(tuning.receipt_confirm_retry_sleep).await;
                    }
                }
            }
        }

        Err(SettlementConfirmationError::Receipt(
            last_error.expect("receipt retry loop should record an error"),
        ))
    }

    pub(crate) async fn settlement_receipt_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<SettlementReceiptStatus>> {
        let receipt = self
            .rpc(
                "eth_getTransactionReceipt",
                json!([format!("{tx_hash:#x}")]),
            )
            .await?;
        if receipt.is_null() {
            return Ok(None);
        }

        let status = receipt
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("transaction receipt did not include status"))?;
        match parse_hex_u64(status)? {
            0 => Ok(Some(SettlementReceiptStatus::Reverted)),
            1 => Ok(Some(SettlementReceiptStatus::Succeeded)),
            value => Err(anyhow!("unexpected transaction receipt status {value}")),
        }
    }

    pub(crate) async fn block_number(&self) -> Result<u64> {
        let value = self.rpc("eth_blockNumber", json!([])).await?;
        parse_hex_u64(
            value
                .as_str()
                .ok_or_else(|| anyhow!("eth_blockNumber was not a hex string"))?,
        )
    }

    pub(crate) async fn dirty_users_from_logs(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<DirtyUserEvent>> {
        if from_block > to_block {
            return Ok(Vec::new());
        }

        let filter = json!([{
            "fromBlock": hex_block(from_block),
            "toBlock": hex_block(to_block),
            "address": [
                format!("{:#x}", self.token_address),
                format!("{:#x}", self.vault_address)
            ],
            "topics": [[
                self.transfer_topic,
                self.match_topic,
                self.withdraw_topic
            ]]
        }]);

        let value = self.rpc("eth_getLogs", filter).await?;
        let logs: Vec<RpcLog> =
            serde_json::from_value(value).context("invalid eth_getLogs response")?;
        let mut users = HashMap::<Address, u64>::new();
        let transfer_topic = self.transfer_topic.to_ascii_lowercase();
        let match_topic = self.match_topic.to_ascii_lowercase();
        let withdraw_topic = self.withdraw_topic.to_ascii_lowercase();

        for log in logs {
            let Some(topic0) = log.topics.first().map(|t| t.to_ascii_lowercase()) else {
                continue;
            };

            let block = log
                .block_number
                .as_deref()
                .and_then(|block| parse_hex_u64(block).ok())
                .unwrap_or(to_block);

            if topic0 == transfer_topic || topic0 == match_topic {
                collect_indexed_address_at_block(&log, 1, block, &mut users);
                collect_indexed_address_at_block(&log, 2, block, &mut users);
            } else if topic0 == withdraw_topic {
                collect_indexed_address_at_block(&log, 1, block, &mut users);
            }
        }

        Ok(users
            .into_iter()
            .map(|(user, block)| DirtyUserEvent { user, block })
            .collect())
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        };
        let response = self
            .http
            .post(self.rpc_url.clone())
            .json(&request)
            .send()
            .await
            .with_context(|| format!("rpc {method} request failed"))?
            .error_for_status()
            .with_context(|| format!("rpc {method} returned an http error"))?
            .json::<JsonRpcResponse>()
            .await
            .with_context(|| format!("rpc {method} response decode failed"))?;

        if let Some(error) = response.error {
            return Err(anyhow!("rpc {method} error: {error}"));
        }
        response
            .result
            .ok_or_else(|| anyhow!("rpc {method} response did not include result"))
    }

    async fn rpc_batch(&self, requests: &[JsonRpcRequest<'_>]) -> Result<HashMap<u64, Value>> {
        let responses = self
            .http
            .post(self.rpc_url.clone())
            .json(requests)
            .send()
            .await
            .context("rpc batch request failed")?
            .error_for_status()
            .context("rpc batch returned an http error")?
            .json::<Vec<JsonRpcResponse>>()
            .await
            .context("rpc batch response decode failed")?;

        let mut values = HashMap::with_capacity(responses.len());
        for response in responses {
            let id = response
                .id
                .ok_or_else(|| anyhow!("rpc batch response did not include id"))?;
            if let Some(error) = response.error {
                return Err(anyhow!("rpc batch id={id} error: {error}"));
            }
            let result = response
                .result
                .ok_or_else(|| anyhow!("rpc batch id={id} response did not include result"))?;
            values.insert(id, result);
        }
        Ok(values)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BalanceRead {
    pub(crate) real: U256,
    pub(crate) vault: U256,
    pub(crate) block: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DirtyUserEvent {
    pub(crate) user: Address,
    pub(crate) block: u64,
}

#[derive(Debug, Error)]
pub(crate) enum SettlementConfirmationError {
    #[error("matchOrders receipt failed: {0:#}")]
    Receipt(#[source] anyhow::Error),
    #[error("matchOrders transaction reverted")]
    Reverted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettlementReceiptStatus {
    Succeeded,
    Reverted,
}

impl SettlementConfirmationError {
    pub(crate) fn outcome_is_uncertain(&self) -> bool {
        matches!(self, Self::Receipt(_))
    }
}

#[derive(Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Deserialize)]
struct JsonRpcResponse {
    id: Option<u64>,
    result: Option<Value>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct RpcLog {
    topics: Vec<String>,
    #[serde(rename = "blockNumber")]
    block_number: Option<String>,
}

fn event_topic(signature: &str) -> String {
    format!("{:#x}", keccak256(signature.as_bytes()))
}

fn hex_block(block: u64) -> String {
    format!("0x{block:x}")
}

fn parse_hex_u64(value: &str) -> Result<u64> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    u64::from_str_radix(value, 16).context("failed to parse hex u64")
}

fn parse_hex_u256(value: &str) -> Result<U256> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    if value.is_empty() {
        return Ok(U256::ZERO);
    }
    U256::from_str_radix(value, 16).context("failed to parse hex u256")
}

fn balance_of_call_data(user: Address) -> String {
    let user = format!("{:#x}", user);
    let user = user.strip_prefix("0x").unwrap_or(&user);
    format!("0x70a08231{user:0>64}")
}

fn collect_indexed_address_at_block(
    log: &RpcLog,
    topic_index: usize,
    block: u64,
    users: &mut HashMap<Address, u64>,
) {
    if let Some(topic) = log.topics.get(topic_index)
        && let Some(user) = address_from_topic(topic)
    {
        users
            .entry(user)
            .and_modify(|current| *current = (*current).max(block))
            .or_insert(block);
    }
}

fn address_from_topic(topic: &str) -> Option<Address> {
    let hex = topic.strip_prefix("0x").unwrap_or(topic);
    if hex.len() != 64 {
        return None;
    }
    Address::from_str(&format!("0x{}", &hex[24..])).ok()
}

#[cfg(test)]
#[path = "chain_tests.rs"]
mod tests;
