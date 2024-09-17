use std::fmt::{Debug, Display, Formatter};
use std::ops::Deref;

use ::cosmwasm_schema::serde;
use alloy_primitives::{Address, AddressError};

use cosmwasm_std::{Addr, StdResult};
use cw_storage_plus::{Key, KeyDeserialize, Prefixer, PrimaryKey};
use thiserror::Error;

/// Valid lengths of decoded addresses
pub const VALID_ADDR_LENGTH: usize = 20;

// Note: this is expanded cw_serde macro minus the Debug implementation, as we want to use Display there
#[derive(::std::clone::Clone, ::std::cmp::PartialEq, ::cosmwasm_schema::schemars::JsonSchema)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[serde(deny_unknown_fields, crate = "::cosmwasm_schema::serde")]
#[schemars(crate = "::cosmwasm_schema::schemars")]
#[derive(Hash, Eq)]
pub struct AccountId(Vec<u8>);

impl Deref for AccountId {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0.as_slice()
    }
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum AccountIdError {
    /// FIXME: normalize this, so we don't have possibly non-deterministic errors from different crate versions
    #[error("Address: {0}")]
    Address(String),

    #[error("Invalid variant: bech32m")]
    InvalidVariant,

    #[error("Invalid prefix: {0} expected {1}")]
    InvalidPrefix(String, &'static str),

    #[error("Invalid address size: {0} bytes")]
    InvalidLength(usize),
}

impl From<AddressError> for AccountIdError {
    fn from(value: AddressError) -> Self {
        AccountIdError::Address(value.to_string())
    }
}

impl Display for AccountId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let addr = Address(self.0.as_slice().try_into().unwrap());
        write!(f, "{}", addr)
    }
}

impl Debug for AccountId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

impl serde::Serialize for AccountId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for AccountId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        AccountId::parse_string(&s).map_err(|e| serde::de::Error::custom(e.to_string()))
    }
}

impl From<&AccountId> for String {
    fn from(value: &AccountId) -> Self {
        value.to_string()
    }
}

impl From<AccountId> for Addr {
    fn from(value: AccountId) -> Self {
        Addr::unchecked(value.to_string())
    }
}

impl From<&AccountId> for Addr {
    fn from(value: &AccountId) -> Self {
        Addr::unchecked(value.to_string())
    }
}

/// This is meant as a helper for testcode
/// Panics on error
pub fn must_id(str: &str) -> AccountId {
    AccountId::parse_string(str).unwrap()
}

impl AccountId {
    /// This takes
    pub fn new(raw: &[u8]) -> Result<Self, AccountIdError> {
        if raw.len() != VALID_ADDR_LENGTH {
            Err(AccountIdError::InvalidLength(raw.len()))
        } else {
            Ok(AccountId(raw.to_vec()))
        }
    }

    // This requires checksumed....
    pub fn parse_string(encoded: &str) -> Result<Self, AccountIdError> {
        let addr = Address::parse_checksummed(encoded, None)?;
        Ok(AccountId(addr.0.to_vec()))
    }

    // only for use in test
    pub fn unchecked(name: &str) -> Self {
        // pad to valid length
        let mut v = name.as_bytes().to_vec();
        v.resize(VALID_ADDR_LENGTH, 0u8);
        AccountId(v)
    }

    pub fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl<'a> PrimaryKey<'a> for AccountId {
    type Prefix = ();
    type SubPrefix = ();
    type Suffix = Self;
    type SuperSuffix = Self;

    fn key(&self) -> Vec<Key> {
        vec![Key::Ref(self.deref())]
    }
}

impl<'a> Prefixer<'a> for AccountId {
    fn prefix(&self) -> Vec<Key> {
        vec![Key::Ref(self.deref())]
    }
}

impl KeyDeserialize for AccountId {
    // const KEY_ELEMS: u16 = 1;
    type Output = AccountId;

    #[inline(always)]
    fn from_vec(value: Vec<u8>) -> StdResult<Self::Output> {
        Ok(AccountId(value))
    }
}

impl KeyDeserialize for &AccountId {
    // const KEY_ELEMS: u16 = 1;
    type Output = AccountId;

    #[inline(always)]
    fn from_vec(value: Vec<u8>) -> StdResult<Self::Output> {
        Ok(AccountId(value))
    }
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::{from_json, to_json_binary};

    use super::*;

    #[test]
    fn test_creation() {
        // we can encode and decode valid addresses
        let id = AccountId::new(&[42u8; 20]).unwrap();
        assert!(id.to_string().starts_with("0x"));
        let reparse = AccountId::parse_string(&id.to_string()).unwrap();
        assert_eq!(id, reparse);

        // enforces valid size (we reject 32 bytes)
        let _ = AccountId::new(&[69u8; 32]).unwrap_err();

        // incorrect raw input fails
        let _ = AccountId::new(&[69u8; 15]).unwrap_err();
    }

    #[test]
    fn test_json_encoding() {
        let raw = [42u8; 20];
        let id = AccountId::new(&raw).unwrap();

        let as_string = to_json_binary(&id.to_string()).unwrap();
        assert!(as_string.starts_with(br#""0x"#));

        // ensure we can decode back to the same value from string
        let parsed: AccountId = from_json(&as_string).unwrap();
        assert_eq!(parsed, id);

        // ensure we encode as string
        let encoded = to_json_binary(&id).unwrap();
        assert_eq!(encoded, as_string);

        // we no longer accept raw
        let as_raw = to_json_binary(&raw).unwrap();
        assert!(as_raw.starts_with(b"[42,42,"));
        let _ = from_json::<AccountId>(&as_raw).unwrap_err();
    }
}
