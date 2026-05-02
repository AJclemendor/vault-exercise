use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use anyhow::Result;
use futures::stream::{self, StreamExt};

use super::chain;
use super::config::Config;
use super::contracts::MockToken;
use super::wallet::{self, User};

const USER_COUNT: usize = 200;
const ETH_PER_USER: u128 = 1_000_000_000_000_000_000;
const TOKENS_PER_USER: u128 = 1_000_000_000_000_000_000_000_000;
const BATCH_SIZE: usize = 50;

pub async fn run(config: &Config) -> Result<Vec<User>> {
    let deployer = wallet::parse(&config.deployer_key);
    let token_addr: Address = config.token_address.parse()?;
    let vault_addr: Address = config.vault_address.parse()?;

    let users = wallet::generate(USER_COUNT);
    println!("Generated {USER_COUNT} user keypairs");

    fund_eth(config, deployer.clone(), &users).await?;
    mint_tokens(config, deployer, token_addr, &users).await?;
    approve_vault(config, token_addr, vault_addr, &users).await?;

    Ok(users)
}

async fn fund_eth(
    config: &Config,
    deployer: alloy::signers::local::PrivateKeySigner,
    users: &[User],
) -> Result<()> {
    let provider = chain::provider(&config.rpc_url, deployer);
    let value = U256::from(ETH_PER_USER);

    for chunk in users.chunks(BATCH_SIZE) {
        let mut pending = Vec::with_capacity(chunk.len());
        for user in chunk {
            let tx = TransactionRequest::default()
                .with_to(user.address)
                .with_value(value);
            pending.push(provider.send_transaction(tx).await?);
        }
        for p in pending {
            p.get_receipt().await?;
        }
    }

    println!("Funded {} users with 1 ETH each", users.len());
    Ok(())
}

async fn mint_tokens(
    config: &Config,
    deployer: alloy::signers::local::PrivateKeySigner,
    token_addr: Address,
    users: &[User],
) -> Result<()> {
    let provider = chain::provider(&config.rpc_url, deployer);
    let token = MockToken::new(token_addr, &provider);
    let amount = U256::from(TOKENS_PER_USER);

    for chunk in users.chunks(BATCH_SIZE) {
        let mut pending = Vec::with_capacity(chunk.len());
        for user in chunk {
            pending.push(token.mint(user.address, amount).send().await?);
        }
        for p in pending {
            p.watch().await?;
        }
    }

    println!("Minted {} tokens to {} users", TOKENS_PER_USER / 10u128.pow(18), users.len());
    Ok(())
}

async fn approve_vault(
    config: &Config,
    token_addr: Address,
    vault_addr: Address,
    users: &[User],
) -> Result<()> {
    let max = U256::MAX;

    stream::iter(users)
        .map(|user| {
            let rpc_url = config.rpc_url.clone();
            let signer = user.signer.clone();
            async move {
                let provider = chain::provider(&rpc_url, signer);
                let token = MockToken::new(token_addr, &provider);
                token.approve(vault_addr, max).send().await?.watch().await?;
                Ok::<_, anyhow::Error>(())
            }
        })
        .buffer_unordered(BATCH_SIZE)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    println!("Approved vault for {} users", users.len());
    Ok(())
}
