use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub rpc_url: String,
    pub ws_url: String,
    pub chain_id: u64,
    pub token_address: String,
    pub vault_address: String,
    pub deployer_key: String,
    pub operator_key: String,
}

impl Config {
    pub fn load(path: &Path) -> Config {
        let contents = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        serde_json::from_str(&contents)
            .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
    }
}
