use flate2::read::GzDecoder;
use layer_std::MsgError;
use std::io::prelude::*;
use thiserror::Error;

/// magic bytes to identify gzip.
/// See https://www.ietf.org/rfc/rfc1952.txt
const GZIP_IDENT: &[u8; 3] = b"\x1F\x8B\x08";

/// magic number for Wasm is "\0asm"
/// See https://webassembly.github.io/spec/core/binary/modules.html#binary-module
const WASM_IDENT: &[u8; 4] = b"\x00\x61\x73\x6D";

#[derive(Error, Debug)]
pub enum ZipError {
    // TODO: need to be careful to convert this for determinism
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Size limit exceeded: {0} bytes")]
    LimitExceeded(usize),

    #[error("Not Gzipped Wasm, not uncompressed wasm")]
    InvalidWasmFormat,
}

impl From<ZipError> for MsgError {
    fn from(e: ZipError) -> Self {
        // TODO: clean up ZipError::Io
        MsgError::ParseError(e.to_string())
    }
}

pub fn unzip(zipped: &[u8], limit: usize) -> Result<Vec<u8>, ZipError> {
    if zipped.len() > limit {
        return Err(ZipError::LimitExceeded(limit));
    }
    let unzip = GzDecoder::new(zipped);
    let over_limit = limit + 1;
    let mut unzipped = Vec::with_capacity(over_limit);
    unzip.take(over_limit as u64).read_to_end(&mut unzipped)?;
    if unzipped.len() > limit {
        return Err(ZipError::LimitExceeded(limit));
    }
    Ok(unzipped)
}

pub fn is_gzip(data: &[u8]) -> bool {
    data.len() >= 3 && data[0..3] == *GZIP_IDENT
}

pub fn is_wasm(data: &[u8]) -> bool {
    data.len() >= 4 && data[0..4] == *WASM_IDENT
}

pub fn unzip_if_needed(maybe_zipped: Vec<u8>, limit: usize) -> Result<Vec<u8>, ZipError> {
    let maybe_wasm = if is_gzip(&maybe_zipped) {
        unzip(&maybe_zipped, limit)?
    } else {
        maybe_zipped
    };
    if is_wasm(&maybe_wasm) {
        Ok(maybe_wasm)
    } else {
        Err(ZipError::InvalidWasmFormat)
    }
}
