mod chain;
mod engine;
mod routes;
mod stats;
mod tasks;
mod types;

use crate::chain::ChainClient;
use crate::engine::Engine;
use anyhow::{Context, Result};
use axum::Router;
use axum::routing::{delete, get, post};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinError;

const LISTEN_ADDR: &str = "127.0.0.1:8080";

#[derive(Debug, Clone, Deserialize)]
struct Config {
    rpc_url: String,
    token_address: String,
    vault_address: String,
    operator_key: String,
}

impl Config {
    fn load() -> Result<Self> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("service crate has no parent directory")?;
        let path = root.join("config/local.json");
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) engine: Arc<Mutex<Engine>>,
    pub(crate) chain: ChainClient,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load()?;
    let chain = ChainClient::new(
        config.rpc_url,
        config.token_address,
        config.vault_address,
        config.operator_key,
    )?;
    let state = AppState {
        engine: Arc::new(Mutex::new(Engine::new())),
        chain,
    };

    spawn_background_tasks(state.clone());

    let app = Router::new()
        .route(
            "/orders",
            post(routes::submit_order).get(routes::list_orders),
        )
        .route("/orders/{id}", delete(routes::cancel_order))
        .route("/balances/{address}", get(routes::get_balance))
        .route("/book", get(routes::get_book))
        .route("/stats", get(routes::get_stats))
        .with_state(state);

    let addr: SocketAddr = LISTEN_ADDR.parse().expect("valid listen address");
    println!("service listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn spawn_background_tasks(state: AppState) {
    let settlement = tokio::spawn(tasks::settlement_loop(state.clone()));
    let active_refresh = tokio::spawn(tasks::active_refresh_loop(state.clone()));
    let log_poll = tokio::spawn(tasks::log_poll_loop(state.clone()));
    let stats_log = tokio::spawn(tasks::stats_log_loop(state));

    tokio::spawn(async move {
        tokio::select! {
            result = settlement => task_finished("settlement_loop", result),
            result = active_refresh => task_finished("active_refresh_loop", result),
            result = log_poll => task_finished("log_poll_loop", result),
            result = stats_log => task_finished("stats_log_loop", result),
        }
    });
}

fn task_finished(name: &str, result: std::result::Result<(), JoinError>) -> ! {
    match result {
        Ok(()) => eprintln!("[fatal] background task {name} exited unexpectedly"),
        Err(err) => eprintln!("[fatal] background task {name} failed: {err}"),
    }
    std::process::exit(1);
}
