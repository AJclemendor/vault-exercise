pub mod actions;
pub mod chain;
pub mod chain_loop;
pub mod config;
pub mod contracts;
pub mod generators;
pub mod order_loop;
pub mod price;
pub mod setup;
pub mod wallet;

#[derive(Clone)]
pub struct HarnessClients {
    pub service: reqwest::Client,
    pub rpc: alloy::transports::http::reqwest::Client,
}
