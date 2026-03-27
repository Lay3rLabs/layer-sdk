use layer_std::AccountId;
use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum AuthError {
    #[error("Invalid Signature")]
    InvalidSignature,

    #[error("Cannot create account {0}, one already exists at this address")]
    AccountExists(AccountId),
}
