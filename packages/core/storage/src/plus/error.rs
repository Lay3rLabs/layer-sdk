use cosmwasm_std::{OverflowError, StdError};
use layer_std::GasError;
use thiserror::Error;

pub type PlusResult<T> = Result<T, PlusError>;

#[derive(Error, Debug, PartialEq)]
pub enum PlusError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Gas(#[from] GasError),
}

impl From<OverflowError> for PlusError {
    fn from(err: OverflowError) -> Self {
        PlusError::Std(err.into())
    }
}
