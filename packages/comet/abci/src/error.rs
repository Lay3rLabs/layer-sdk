use thiserror::Error;

#[derive(Error, Debug)]
pub enum AbciError {
    #[error("{0}")]
    Io(#[from] tokio::io::Error),

    #[error("{0}")]
    Encode(#[from] prost::EncodeError),

    #[error("{0}")]
    Decode(#[from] prost::DecodeError),
}
