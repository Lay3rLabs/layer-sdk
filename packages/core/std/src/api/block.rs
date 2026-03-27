use cosmwasm_std::{Event, Timestamp};

use super::{ConsensusParams, TxResult, Validator, ValidatorUpdate};
use crate::Tx;

/*
    pub struct RequestFinalizeBlock {
    #[prost(bytes = "bytes", repeated, tag = "1")]
    pub txs: ::prost::alloc::vec::Vec<::prost::bytes::Bytes>,
    #[prost(message, optional, tag = "2")]
    pub decided_last_commit: ::core::option::Option<CommitInfo>,
    #[prost(message, repeated, tag = "3")]
    pub misbehavior: ::prost::alloc::vec::Vec<Misbehavior>,
    /// hash is the merkle root hash of the fields of the decided block.
    #[prost(bytes = "bytes", tag = "4")]
    pub hash: ::prost::bytes::Bytes,
    #[prost(int64, tag = "5")]
    pub height: i64,
    #[prost(message, optional, tag = "6")]
    pub time: ::core::option::Option<crate::google::protobuf::Timestamp>,
    #[prost(bytes = "bytes", tag = "7")]
    pub next_validators_hash: ::prost::bytes::Bytes,
    /// proposer_address is the address of the public key of the original proposer of the block.
    #[prost(bytes = "bytes", tag = "8")]
    pub proposer_address: ::prost::bytes::Bytes,
}
     */

// See https://github.com/informalsystems/tendermint-rs/blob/mikhail/cometbft-0.38/proto/src/prost/v0_38/tendermint.abci.rs#L220-L239
pub struct Block {
    pub txs: Vec<Tx>,
    pub height: u64,
    pub time: Timestamp,

    /// proposer_address is the address of the public key of the original proposer of the block.
    pub proposer_address: Vec<u8>,
    /// votes contains all validators who voted for the last block to make consensus
    pub last_votes: Vec<Validator>,
}

/**
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ResponseFinalizeBlock {
    /// set of block events emmitted as part of executing the block
    #[prost(message, repeated, tag = "1")]
    pub events: ::prost::alloc::vec::Vec<Event>,
    /// the result of executing each transaction including the events
    /// the particular transction emitted. This should match the order
    /// of the transactions delivered in the block itself
    #[prost(message, repeated, tag = "2")]
    pub tx_results: ::prost::alloc::vec::Vec<ExecTxResult>,
    /// a list of updates to the validator set. These will reflect the validator set at current height + 2.
    #[prost(message, repeated, tag = "3")]
    pub validator_updates: ::prost::alloc::vec::Vec<ValidatorUpdate>,
    /// updates to the consensus params, if any.
    #[prost(message, optional, tag = "4")]
    pub consensus_param_updates: ::core::option::Option<super::types::ConsensusParams>,
    /// app_hash is the hash of the applications' state which is used to confirm that execution of the transactions was deterministic. It is up to the application to decide which algorithm to use.
    #[prost(bytes = "bytes", tag = "5")]
    pub app_hash: ::prost::bytes::Bytes,
}
**/

pub struct FinalizeBlockResponse<E: std::error::Error> {
    /// set of block events emmitted as part of executing the block
    pub events: Vec<Event>,

    /// the result of executing each transaction including the events
    /// the particular transction emitted. This should match the order
    /// of the transactions delivered in the block itself
    pub tx_results: Vec<TxResult<E>>,

    /// a list of updates to the validator set. These will reflect the validator set at current height + 2.
    pub validator_updates: Vec<ValidatorUpdate>,

    /// updates to the consensus params, if any.
    pub consensus_param_updates: Option<ConsensusParams>,

    /// app_hash is the hash of the applications' state which is used to confirm that execution of the transactions was deterministic. It is up to the application to decide which algorithm to use.
    pub app_hash: Vec<u8>,
}
