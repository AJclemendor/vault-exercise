use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::SmallRng;
use std::sync::Arc;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use super::actions;
use super::config::Config;
use super::wallet::User;

pub fn spawn_all<P: Provider + Clone + Send + Sync + 'static>(
    users: &[User],
    config: Arc<Config>,
    reader: P,
    token_address: Address,
    vault_address: Address,
    cancel: CancellationToken,
    client: reqwest::Client,
    master_seed: u64,
) -> Vec<tokio::task::JoinHandle<()>> {
    let all_addresses: Vec<Address> = users.iter().map(|u| u.address).collect();

    users
        .iter()
        .enumerate()
        .map(|(i, user)| {
            let address = user.address;
            let signer = user.signer.clone();
            let config = config.clone();
            let reader = reader.clone();
            let cancel = cancel.clone();
            let client = client.clone();
            let peers = all_addresses.clone();
            tokio::spawn(run_one(
                i,
                address,
                signer,
                config,
                reader,
                token_address,
                vault_address,
                cancel,
                client,
                peers,
                master_seed,
            ))
        })
        .collect()
}

async fn run_one<P: Provider>(
    index: usize,
    address: Address,
    signer: alloy::signers::local::PrivateKeySigner,
    config: Arc<Config>,
    reader: P,
    token_address: Address,
    vault_address: Address,
    cancel: CancellationToken,
    client: reqwest::Client,
    peers: Vec<Address>,
    master_seed: u64,
) {
    let mut rng =
        SmallRng::seed_from_u64(master_seed.wrapping_add(index as u64).wrapping_add(10_000));
    let user = User { address, signer };

    loop {
        let sleep_ms: u64 = rng.random_range(800..=1500);
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {}
        }

        let vault_balance = match actions::get_vault_balance(&reader, vault_address, address).await
        {
            Ok(b) => b,
            Err(e) => {
                println!("[chain] user={} vault balance query failed: {e}", address);
                continue;
            }
        };

        if !vault_balance.is_zero() {
            match actions::withdraw_all_from_vault(&reader, &config, &client, &user, vault_address)
                .await
            {
                Ok(amount) => {
                    if !amount.is_zero() {
                        println!("[chain] user={} withdraw_all amount={}", address, amount);
                    }
                }
                Err(e) => {
                    println!("[chain] user={} withdraw_all -> error: {e}", address);
                }
            }
        }

        if peers.len() > 1 && rng.random_bool(0.30) {
            let mut idx = rng.random_range(0..peers.len());
            if idx == index {
                idx = (idx + 1) % peers.len();
            }
            let target = peers[idx];

            let eoa = match actions::get_eoa_balance(&reader, token_address, address).await {
                Ok(b) => b,
                Err(e) => {
                    println!("[chain] user={} eoa balance query failed: {e}", address);
                    continue;
                }
            };
            if !eoa.is_zero() {
                let pct = rng.random_range(10u64..=40u64);
                let amount = eoa * U256::from(pct) / U256::from(100u64);
                if !amount.is_zero() {
                    match actions::transfer_tokens(
                        &config,
                        &client,
                        &user,
                        target,
                        token_address,
                        amount,
                    )
                    .await
                    {
                        Ok(()) => {
                            println!(
                                "[chain] user={} transfer_to_peer target={} amount={}",
                                address, target, amount,
                            );
                        }
                        Err(e) => {
                            println!(
                                "[chain] user={} transfer_to_peer target={} amount={} -> error: {e}",
                                address, target, amount,
                            );
                        }
                    }
                }
            }
        }
    }
}
