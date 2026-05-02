use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;

pub struct User {
    pub address: Address,
    pub signer: PrivateKeySigner,
}

impl User {
    pub fn from_signer(signer: PrivateKeySigner) -> Self {
        Self {
            address: signer.address(),
            signer,
        }
    }
}

pub fn generate(n: usize) -> Vec<User> {
    (0..n)
        .map(|_| User::from_signer(PrivateKeySigner::random()))
        .collect()
}

pub fn parse(hex_key: &str) -> PrivateKeySigner {
    hex_key.parse().expect("invalid private key")
}
