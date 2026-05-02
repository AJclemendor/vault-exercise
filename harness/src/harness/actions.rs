use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use anyhow::Result;

use super::chain;
use super::config::Config;
use super::contracts::{MockToken, Vault};
use super::wallet::User;

pub async fn transfer_tokens(
    config: &Config,
    client: &reqwest::Client,
    from: &User,
    to: Address,
    token_address: Address,
    amount: U256,
) -> Result<()> {
    let provider = chain::provider(&config.rpc_url, client.clone(), from.signer.clone());
    let token = MockToken::new(token_address, &provider);
    token.transfer(to, amount).send().await?.watch().await?;
    Ok(())
}

pub async fn withdraw_all_from_vault<P: Provider>(
    reader: &P,
    config: &Config,
    client: &reqwest::Client,
    user: &User,
    vault_address: Address,
) -> Result<U256> {
    let balance = get_vault_balance(reader, vault_address, user.address).await?;

    if balance.is_zero() {
        return Ok(U256::ZERO);
    }

    let provider = chain::provider(&config.rpc_url, client.clone(), user.signer.clone());
    let vault = Vault::new(vault_address, &provider);
    vault.withdraw(balance).send().await?.watch().await?;
    Ok(balance)
}

pub async fn get_eoa_balance<P: Provider>(
    reader: &P,
    token_address: Address,
    user_address: Address,
) -> Result<U256> {
    let token = MockToken::new(token_address, reader);
    let balance = token.balanceOf(user_address).call().await?;
    Ok(balance)
}

pub async fn get_vault_balance<P: Provider>(
    reader: &P,
    vault_address: Address,
    user_address: Address,
) -> Result<U256> {
    let vault = Vault::new(vault_address, reader);
    let balance = vault.balanceOf(user_address).call().await?;
    Ok(balance)
}
