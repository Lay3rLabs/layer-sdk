#[derive(Debug, Clone, PartialEq)]
pub struct Validator {
    /// The first 20 bytes of SHA256(public key)
    pub address: Vec<u8>,
    /// The voting power
    pub power: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatorUpdate {
    pub pub_key: TmPubKey,
    /// The voting power
    pub power: u64,
}

/// Possible public keys of validator nodes
#[derive(Debug, Clone, PartialEq)]
pub enum TmPubKey {
    Ed25519(Vec<u8>),
    Secp256k1(Vec<u8>),
}

impl TmPubKey {
    /// The first 20 bytes of SHA256(public key)
    /// TODO: is this raw pubkey or do we need type info there serialized somehow???
    pub fn address(&self) -> Vec<u8> {
        todo!()
    }
}

#[allow(unused)]
pub enum TmPubKeyType {
    Ed25519,
    Secp256k1,
}
