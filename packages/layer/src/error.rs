use cosmwasm_std::StdError;
use layer_std::{AccountIdError, GasError, QueryError, TxError};
use layer_storage::PlusError;
use thiserror::Error;

use crate::auth::AuthError;
use crate::bank::BankError;
use crate::wasm::WasmError;

pub type PulsarResult<T> = Result<T, PulsarError>;

#[derive(Error, Debug, PartialEq)]
pub enum PulsarError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Auth(#[from] AuthError),

    #[error("{0}")]
    AccountId(#[from] AccountIdError),

    #[error("{0}")]
    Bank(#[from] BankError),

    #[error("{0}")]
    Wasm(#[from] WasmError),

    #[error("{0}")]
    Query(#[from] QueryError),

    #[error("{0}")]
    Tx(#[from] TxError),

    #[error("{0}")]
    Gas(#[from] GasError),

    #[error("Unexpected block height. Got {got}, previous {previous}")]
    BadBlockHeight { got: u64, previous: u64 },

    #[error("Descending block time. Got {got}, previous {previous}")]
    DescendingBlockTime { got: u64, previous: u64 },

    #[error("Tx requested more gas than remaining in block. Requested {requested}, remaining {remaining}")]
    ExceedsRemainingBlockGas { requested: u64, remaining: u64 },
}

impl From<PlusError> for PulsarError {
    fn from(value: PlusError) -> Self {
        match value {
            PlusError::Gas(err) => PulsarError::Gas(err),
            PlusError::Std(err) => PulsarError::Std(err),
        }
    }
}

impl From<PulsarError> for cw_orch_core::CwEnvError {
    fn from(value: PulsarError) -> Self {
        anyhow::Error::new(value).into()
    }
}
