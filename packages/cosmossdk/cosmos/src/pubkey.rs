use cosmrs::tendermint::PublicKey as TendermintPublicKey;
use cosmrs::tx::SignerPublicKey;
use cosmrs::Any;

use layer_std::{PubKey, QueryError, TxError};

pub fn parse_cosmos_pubkey(pubkey: &SignerPublicKey) -> Result<PubKey, TxError> {
    match pubkey {
        SignerPublicKey::Single(pk) => match pk.type_url() {
            cosmrs::crypto::PublicKey::ED25519_TYPE_URL => Ok(PubKey::ed25519(pk.to_bytes())),
            cosmrs::crypto::PublicKey::SECP256K1_TYPE_URL => Ok(PubKey::secp256k1(pk.to_bytes())),
            url => Err(TxError::UnsupportedPubKey(url)),
        },
        _ => Err(TxError::UnsupportedPubKey("multisig")),
    }
}

pub fn encode_cosmos_pubkey(pubkey: &PubKey) -> Result<Any, QueryError> {
    let pk = match pubkey {
        PubKey::Ed25519(pk) => TendermintPublicKey::from_raw_ed25519(pk),
        PubKey::Secp256k1(pk) => TendermintPublicKey::from_raw_secp256k1(pk),
    }
    .ok_or_else(|| QueryError::EncodingError("invalid pubkey".to_string()))?;

    // FIXME: map error to some generic line not the undeterministic report line
    cosmrs::crypto::PublicKey::from(pk)
        .to_any()
        .map_err(|e| QueryError::EncodingError(e.to_string()))
}
