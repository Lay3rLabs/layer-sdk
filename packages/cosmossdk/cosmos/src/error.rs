use cosmwasm_std::StdError;
use layer_std::{MsgError, QueryError, TxError};
use thiserror::Error;

use cosmos_sdk_proto::prost::DecodeError;
use cosmrs::ErrorReport;

/// This just serves as an intermediate between cosmrs and prost level errors.
/// And the layer_std::{MsgError, TxError} types.
///
/// We don't just want to wrap them or convert to string, but provide a level
/// to ensure we don't leak (non-deterministic) internal errors to the rest of the app.
#[derive(Error, Debug, PartialEq)]
pub enum CosmosError {
    #[error("{0}")]
    Std(#[from] StdError),

    // FIXME: remove this and replace with deterministic errors
    #[error("Prost: {0}")]
    ProtoDecode(#[from] DecodeError),

    // FIXME: remove this and replace with deterministic errors
    #[error("Cosmrs: {0}")]
    ErrorReport(String),
}

impl From<ErrorReport> for CosmosError {
    fn from(value: ErrorReport) -> Self {
        CosmosError::ErrorReport(value.to_string())
    }
}

// FIXME: for production, produce a few fixed type-strings (but harder debugging)
impl From<CosmosError> for MsgError {
    fn from(err: CosmosError) -> Self {
        match err {
            CosmosError::Std(e) => MsgError::Std(e),
            CosmosError::ProtoDecode(e) => MsgError::ParseError(e.to_string()),
            CosmosError::ErrorReport(e) => MsgError::ParseError(e),
            // CosmosError::ProtoDecode(e) => MsgError::ParseError("prost".to_string()),
            // CosmosError::ErrorReport(e) => MsgError::ParseError("cosmrs".to_string()),
        }
    }
}

// FIXME: for production, produce a few fixed type-strings (but harder debugging)
impl From<CosmosError> for TxError {
    fn from(err: CosmosError) -> Self {
        match err {
            // FIXME: add another variant to TxError?
            CosmosError::Std(e) => TxError::Msg(MsgError::Std(e)),
            CosmosError::ProtoDecode(e) => TxError::ParseError(e.to_string()),
            CosmosError::ErrorReport(e) => TxError::ParseError(e),
        }
    }
}

// FIXME: for production, produce a few fixed type-strings (but harder debugging)
impl From<CosmosError> for QueryError {
    fn from(err: CosmosError) -> Self {
        match err {
            CosmosError::Std(e) => QueryError::Std(e),
            CosmosError::ProtoDecode(e) => QueryError::ParseError(e.to_string()),
            CosmosError::ErrorReport(e) => QueryError::ParseError(e),
        }
    }
}
