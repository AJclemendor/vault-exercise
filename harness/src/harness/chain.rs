use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy::transports::http::reqwest::Client as AlloyHttpClient;
use alloy::transports::http::reqwest::Url as AlloyUrl;

pub fn provider(
    rpc_url: &str,
    client: AlloyHttpClient,
    signer: PrivateKeySigner,
) -> impl alloy::providers::Provider + Clone {
    ProviderBuilder::new().wallet(signer).connect_reqwest(
        client,
        rpc_url.parse::<AlloyUrl>().expect("invalid RPC URL"),
    )
}

pub fn reader(
    rpc_url: &str,
    client: AlloyHttpClient,
) -> impl alloy::providers::Provider + Clone + use<> {
    ProviderBuilder::new().connect_reqwest(
        client,
        rpc_url.parse::<AlloyUrl>().expect("invalid RPC URL"),
    )
}
