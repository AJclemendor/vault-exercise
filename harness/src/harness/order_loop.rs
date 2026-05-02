use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use rand::rngs::SmallRng;
use rand::Rng;
use rand::SeedableRng;
use std::sync::Arc;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use super::actions;
use super::config::Config;
use super::generators::{self, FairPrice};
use super::wallet::User;
use crate::service;

const ONE_TOKEN: U256 = U256::from_limbs([1_000_000_000_000_000_000u64, 0, 0, 0]);

pub fn spawn_all<P: Provider + Clone + Send + Sync + 'static>(
    users: &[User],
    config: Arc<Config>,
    reader: P,
    token_address: Address,
    fair: FairPrice,
    cancel: CancellationToken,
    client: reqwest::Client,
    master_seed: u64,
) -> Vec<tokio::task::JoinHandle<()>> {
    users
        .iter()
        .enumerate()
        .map(|(i, user)| {
            let address = user.address;
            let config = config.clone();
            let reader = reader.clone();
            let fair = fair.clone();
            let cancel = cancel.clone();
            let client = client.clone();
            tokio::spawn(run_one(
                i, address, config, reader, token_address, fair, cancel, client, master_seed,
            ))
        })
        .collect()
}

async fn run_one<P: Provider>(
    index: usize,
    address: Address,
    config: Arc<Config>,
    reader: P,
    token_address: Address,
    fair: FairPrice,
    cancel: CancellationToken,
    client: reqwest::Client,
    master_seed: u64,
) {
    let mut rng = SmallRng::seed_from_u64(master_seed.wrapping_add(index as u64));
    let _ = config;

    loop {
        let sleep_ms: u64 = rng.random_range(800..=1200);
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {}
        }

        let eoa_balance = match actions::get_eoa_balance(&reader, token_address, address).await {
            Ok(b) => b,
            Err(e) => {
                println!("[order] user={} balance query failed: {e}", address);
                continue;
            }
        };

        if eoa_balance < ONE_TOKEN {
            continue;
        }

        let params = generators::pick_order_params(&mut rng, eoa_balance, &fair);

        match service::submit_order(
            &client,
            address,
            params.side,
            params.order_type,
            params.price,
            params.size,
        )
        .await
        {
            Ok(resp) => {
                println!(
                    "[order] user={} side={:?} type={:?} price={} size={} -> id={} status={:?}",
                    address,
                    params.side,
                    params.order_type,
                    params.price,
                    params.size,
                    resp.order_id,
                    resp.status,
                );
            }
            Err(e) => {
                println!(
                    "[order] user={} side={:?} type={:?} price={} size={} -> error: {e}",
                    address, params.side, params.order_type, params.price, params.size,
                );
            }
        }
    }
}
