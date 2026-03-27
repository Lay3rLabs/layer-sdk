//! Encoding/decoding mechanisms for ABCI requests and responses.
//!
//! Implements the [Tendermint Socket Protocol][tsp].
//!
//! [tsp]: https://github.com/tendermint/tendermint/blob/v0.34.x/spec/abci/client-server.md#tsp

use std::marker::PhantomData;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use bytes::{Buf, Bytes, BytesMut};
use prost::Message;
use tendermint_proto::v0_38::abci::{Request, Response};

use crate::error::AbciError;

/// The maximum number of bytes we expect in a varint. We use this to check if
/// we're encountering a decoding error for a varint.
pub const MAX_VARINT_LENGTH: usize = 16;

/// The server receives incoming requests, and sends outgoing responses.
pub type ServerCodec = Codec<Request, Response>;

/// Allows for iteration over `S` to produce instances of `I`, as well as
/// sending instances of `O`.
pub struct Codec<I, O> {
    stream: TcpStream,
    // Long-running read buffer
    read_buf: BytesMut,
    _incoming: PhantomData<I>,
    _outgoing: PhantomData<O>,
}

impl<I, O> Codec<I, O>
where
    I: Message + Default,
    O: Message,
{
    /// Constructor.
    pub fn new(stream: TcpStream, read_buf_size: usize) -> Self {
        Self {
            stream,
            read_buf: BytesMut::with_capacity(read_buf_size),
            _incoming: Default::default(),
            _outgoing: Default::default(),
        }
    }
}

// Read the next input type from the underlying stream.
impl<I, O> Codec<I, O>
where
    I: Message + Default,
{
    pub async fn next(&mut self) -> Option<Result<I, AbciError>> {
        loop {
            // Try to decode an incoming message from our buffer first
            match decode_length_delimited::<I>(&mut self.read_buf) {
                Ok(Some(incoming)) => return Some(Ok(incoming)),
                Ok(None) => (), // not enough data to decode a message, let's continue.
                Err(e) => return Some(Err(e)),
            }

            // If we don't have enough data to decode a message, try to read
            // more
            match self.stream.read_buf(&mut self.read_buf).await {
                Ok(0) => return None,
                Err(e) => return Some(Err(AbciError::Io(e))),
                Ok(_) => {}
            }
        }
    }
}

impl<I, O> Codec<I, O>
where
    O: Message,
{
    /// Send a message using this codec.
    pub async fn send(&mut self, message: O) -> Result<(), AbciError> {
        let data = encode_length_delimited(message)?;
        self.stream.write_all(&data).await.map_err(AbciError::Io)?;
        self.stream.flush().await.map_err(AbciError::Io)?;
        Ok(())
    }
}

/// Encode the given message with a length prefix.
fn encode_length_delimited<M>(message: M) -> Result<Bytes, AbciError>
where
    M: Message,
{
    let data = message.encode_to_vec();
    let mut output = BytesMut::with_capacity(data.len() + MAX_VARINT_LENGTH);
    prost::encoding::encode_varint(data.len() as u64, &mut output);
    output.extend_from_slice(&data);
    Ok(output.freeze())
}

/// Attempt to decode a message of type `M` from the given source buffer.
fn decode_length_delimited<M>(src: &mut BytesMut) -> Result<Option<M>, AbciError>
where
    M: Message + Default,
{
    let src_len = src.len();
    let mut tmp = src.clone().freeze();
    let encoded_len = match prost::encoding::decode_varint(&mut tmp) {
        Ok(len) => len,
        // We've potentially only received a partial length delimiter
        Err(_) if src_len <= MAX_VARINT_LENGTH => return Ok(None),
        Err(e) => return Err(AbciError::Decode(e)),
    };
    let remaining = tmp.remaining() as u64;
    if remaining < encoded_len {
        // We don't have enough data yet to decode the entire message
        Ok(None)
    } else {
        let delim_len = src_len - tmp.remaining();
        // We only advance the source buffer once we're sure we have enough
        // data to try to decode the result.
        src.advance(delim_len + (encoded_len as usize));

        let mut result_bytes = BytesMut::from(tmp.split_to(encoded_len as usize).as_ref());
        let res = M::decode(&mut result_bytes).map_err(AbciError::Decode)?;

        Ok(Some(res))
    }
}
