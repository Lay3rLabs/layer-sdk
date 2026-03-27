use super::utils::Instantiate2AddressError;

use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum WasmError {
    #[error("Cannot send message as another account")]
    SenderMismatch,

    #[error("Unauthorized action")]
    Unauthorized,

    #[error("Used an invalid key in an event attribute: {0}")]
    InvalidAttributeKey(String),

    #[error("VmError: {0}")]
    Vm(String),

    #[error("Contract Error: {0}")]
    Contract(String),

    #[error("Instantiate2: {0}")]
    Instantiate2Error(#[from] Instantiate2AddressError),

    #[error("Invalid Checksum, must be 32 bytes")]
    Checksum,

    #[error("Tried to return SubMsg in sandboxed contract call")]
    SubMsgNotSupported,

    #[error("Only root contract call call custom messages")]
    NotRoot,
}
