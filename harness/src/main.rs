mod harness;
mod service;

use alloy::primitives::Address;
use harness::HarnessClients;
use harness::config::Config;
use harness::generators::FairPrice;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MASTER_SEED: u64 = 42;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let config = Arc::new(Config::load(&root.join("config/local.json")));
    let clients = HarnessClients {
        service: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(512)
            .build()?,
        rpc: alloy::transports::http::reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(512)
            .build()?,
    };
    let users = harness::setup::run(&config, clients.rpc.clone()).await?;
    println!("Setup complete: {} users funded and approved", users.len());

    let token_address: Address = config.token_address.parse()?;
    let vault_address: Address = config.vault_address.parse()?;
    let reader = harness::chain::reader(&config.rpc_url, clients.rpc.clone());

    let fair = FairPrice::new();
    let cancel = CancellationToken::new();

    harness::price::spawn(fair.clone(), cancel.clone());

    let _order_handles = harness::order_loop::spawn_all(
        &users,
        config.clone(),
        reader.clone(),
        token_address,
        fair.clone(),
        cancel.clone(),
        clients.service.clone(),
        MASTER_SEED,
    );

    let _chain_handles = harness::chain_loop::spawn_all(
        &users,
        config.clone(),
        reader,
        clients.rpc.clone(),
        token_address,
        vault_address,
        cancel.clone(),
        MASTER_SEED,
    );

    println!(
        "Simulation running: {} order loops, {} chain loops — Ctrl-C to stop",
        _order_handles.len(),
        _chain_handles.len(),
    );

    if let Ok(run_secs) = std::env::var("HARNESS_RUN_SECS") {
        let run_secs: u64 = run_secs.parse()?;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(run_secs)) => {
                println!("HARNESS_RUN_SECS elapsed; stopping simulation");
                cancel.cancel();
            }
            signal = tokio::signal::ctrl_c() => {
                signal?;
                cancel.cancel();
            }
        }
    } else {
        tokio::signal::ctrl_c().await?;
        cancel.cancel();
    }

    std::process::exit(0);
}
