use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;

pub fn provider(rpc_url: &str, signer: PrivateKeySigner) -> impl alloy::providers::Provider + Clone {
    ProviderBuilder::new()
        .wallet(signer)
        .connect_http(rpc_url.parse().expect("invalid RPC URL"))
}

pub fn reader(rpc_url: &str) -> impl alloy::providers::Provider + Clone + use<> {
    ProviderBuilder::new()
        .connect_http(rpc_url.parse::<alloy::transports::http::reqwest::Url>().expect("invalid RPC URL"))
}
