use sha2::{
    digest::{Digest, Update},
    Sha256,
};
use thiserror::Error;

use layer_std::{AccountId, VALID_ADDR_LENGTH};

use crate::PulsarError;

use super::WasmError;

// 2.0: use cosmwasm_std::Checksum
type Checksum = cosmwasm_std::Binary;

pub fn build_instantiate_address(
    sender: &[u8],
    code_id: u64,
    counter: u64,
) -> Result<AccountId, PulsarError> {
    let mut prehash = Vec::with_capacity(sender.len() + 8 + 8);
    prehash.extend_from_slice(sender);
    prehash.extend(code_id.to_be_bytes());
    prehash.extend(counter.to_be_bytes());
    let raw = Sha256::digest(prehash);
    Ok(AccountId::new(&raw[..VALID_ADDR_LENGTH])?)
}

/// We should match the reference wasmd/Go implementation for compatibility with cosmos-sdk:
///     https://github.com/CosmWasm/wasmd/blob/v0.50.0/x/wasm/keeper/addresses.go#L43-L72
///
/// This algorithm can be found in Rust in the cosmwasm-std crate, which we base on:
///     https://github.com/CosmWasm/cosmwasm/blob/v1.5.0/packages/std/src/addresses.rs#L310-L390
///
/// Note, that we make a few changes for the type system, but keep the logic the same
pub fn build_instantiate_2_address(
    checksum: &Checksum,
    creator: &AccountId,
    salt: &[u8],
    msg: &[u8],
) -> Result<AccountId, PulsarError> {
    if checksum.len() != 32 {
        return Err(Instantiate2AddressError::InvalidChecksumLength.into());
    }
    if salt.is_empty() || salt.len() > 64 {
        return Err(Instantiate2AddressError::InvalidSaltLength.into());
    };

    let mut key = Vec::<u8>::new();
    key.extend_from_slice(b"wasm\0");
    // Fixed length from Checksum type (checked above)
    key.extend_from_slice(&(32u64).to_be_bytes());
    key.extend_from_slice(checksum.as_slice());
    key.extend_from_slice(&(creator.len() as u64).to_be_bytes());
    key.extend_from_slice(creator);
    key.extend_from_slice(&(salt.len() as u64).to_be_bytes());
    key.extend_from_slice(salt);
    key.extend_from_slice(&(msg.len() as u64).to_be_bytes());
    key.extend_from_slice(msg);
    let address_data = hash("module", &key);
    Ok(AccountId::new(&address_data[..VALID_ADDR_LENGTH])?)
}

/// This must be compatible with the wasmd calls, and thus map to address.Module in Cosmos SDK.
///
/// The spec for "Basic Address" Hash can be found here:
/// https://github.com/cosmos/cosmos-sdk/blob/v0.45.8/docs/architecture/adr-028-public-key-addresses.md#module-account-addresses
///
/// The Cosmos SDK implementation of the hash is here:
/// func Module: https://github.com/cosmos/cosmos-sdk/blob/v0.50.0/types/address/hash.go#L66-L86
/// func Hash: https://github.com/cosmos/cosmos-sdk/blob/v0.50.0/types/address/hash.go#L24-L40
fn hash(ty: &str, key: &[u8]) -> Vec<u8> {
    let inner = Sha256::digest(ty.as_bytes());
    Sha256::new().chain(inner).chain(key).finalize().to_vec()
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum Instantiate2AddressError {
    /// Checksum must be 32 bytes
    #[error("invalid checksum length")]
    InvalidChecksumLength,
    /// Salt must be between 1 and 64 bytes
    #[error("invalid salt length")]
    InvalidSaltLength,
}

impl From<Instantiate2AddressError> for PulsarError {
    fn from(value: Instantiate2AddressError) -> Self {
        PulsarError::Wasm(WasmError::Instantiate2Error(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use hex_literal::hex;

    // test vectors from cosmwasm-std
    #[test]
    fn build_instantiate_2_address_works() {
        let checksum1 =
            hex!("13a1fc994cc6d1c81b746ee0c0ff6f90043875e0bf1d9be6b7d779fc978dc2a5").into();
        let creator1 = AccountId::new(&hex!("9999999999aaaaaaaaaabbbbbbbbbbcccccccccc")).unwrap();
        let salt1 = hex!("61");
        let salt2 = hex!("aabbccddeeffffeeddbbccddaa66551155aaaabbcc787878789900aabbccddeeffffeeddbbccddaa66551155aaaabbcc787878789900aabbbbcc221100acadae");
        let msg1: &[u8] = b"";
        let msg2: &[u8] = b"{}";
        let msg3: &[u8] = b"{\"some\":123,\"structure\":{\"nested\":[\"ok\",true]}}";

        // TODO: reduce all expected to 40 chars, not 64 chars

        // No msg
        let expected = AccountId::new(&hex!("5e865d3e45ad3e961f77fd77d46543417ced44d9")).unwrap();
        assert_eq!(
            build_instantiate_2_address(&checksum1, &creator1, &salt1, msg1).unwrap(),
            expected
        );

        // With msg
        let expected = AccountId::new(&hex!("0995499608947a5281e2c7ebd71bdb26a1ad9819")).unwrap();
        assert_eq!(
            build_instantiate_2_address(&checksum1, &creator1, &salt1, msg2).unwrap(),
            expected
        );

        // Long msg
        let expected = AccountId::new(&hex!("83326e554723b15bac664ceabc8a5887e27003ab")).unwrap();
        assert_eq!(
            build_instantiate_2_address(&checksum1, &creator1, &salt1, msg3).unwrap(),
            expected
        );

        // Long salt
        let expected = AccountId::new(&hex!("9384c6248c0bb171e306fd7da0993ec1e20eba00")).unwrap();
        assert_eq!(
            build_instantiate_2_address(&checksum1, &creator1, &salt2, b"").unwrap(),
            expected
        );

        // Salt too short or too long
        let empty = Vec::<u8>::new();
        assert!(matches!(
            build_instantiate_2_address(&checksum1, &creator1, &empty, b"").unwrap_err(),
            PulsarError::Wasm(WasmError::Instantiate2Error(
                Instantiate2AddressError::InvalidSaltLength
            ))
        ));
        let too_long = vec![0x11; 65];
        assert!(matches!(
            build_instantiate_2_address(&checksum1, &creator1, &too_long, b"").unwrap_err(),
            PulsarError::Wasm(WasmError::Instantiate2Error(
                Instantiate2AddressError::InvalidSaltLength
            ))
        ));

        // invalid checksum length
        let broken_cs =
            hex!("13a1fc994cc6d1c81b746ee0c0ff6f90043875e0bf1d9be6b7d779fc978dc2").into();
        assert!(matches!(
            build_instantiate_2_address(&broken_cs, &creator1, &salt1, b"").unwrap_err(),
            PulsarError::Wasm(WasmError::Instantiate2Error(
                Instantiate2AddressError::InvalidChecksumLength
            ))
        ));
        let broken_cs = hex!("").into();
        assert!(matches!(
            build_instantiate_2_address(&broken_cs, &creator1, &salt1, b"").unwrap_err(),
            PulsarError::Wasm(WasmError::Instantiate2Error(
                Instantiate2AddressError::InvalidChecksumLength
            ))
        ));
        let broken_cs =
            hex!("13a1fc994cc6d1c81b746ee0c0ff6f90043875e0bf1d9be6b7d779fc978dc2aaaa").into();
        assert!(matches!(
            build_instantiate_2_address(&broken_cs, &creator1, &salt1, b"").unwrap_err(),
            PulsarError::Wasm(WasmError::Instantiate2Error(
                Instantiate2AddressError::InvalidChecksumLength
            ))
        ));

        // 2.0
        // // invalid checksum length won't even make a Checksum
        // let _ =
        //     Checksum::from_hex("13a1fc994cc6d1c81b746ee0c0ff6f90043875e0bf1d9be6b7d779fc978dc2")
        //         .unwrap_err();
        // let _ = Checksum::from_hex("").unwrap_err();
        // let _ = Checksum::from_hex(
        //     "13a1fc994cc6d1c81b746ee0c0ff6f90043875e0bf1d9be6b7d779fc978dc2aaaa",
        // )
        // .unwrap_err();
    }
}
