/*
    pub struct RequestInitChain {
    #[prost(message, optional, tag = "1")]
    pub time: ::core::option::Option<crate::google::protobuf::Timestamp>,
    #[prost(string, tag = "2")]
    pub chain_id: ::prost::alloc::string::String,
    #[prost(message, optional, tag = "3")]
    pub consensus_params: ::core::option::Option<super::types::ConsensusParams>,
    #[prost(message, repeated, tag = "4")]
    pub validators: ::prost::alloc::vec::Vec<ValidatorUpdate>,
    #[prost(bytes = "bytes", tag = "5")]
    pub app_state_bytes: ::prost::bytes::Bytes,
    #[prost(int64, tag = "6")]
    pub initial_height: i64,
}

https://github.com/informalsystems/tendermint-rs/blob/mikhail/cometbft-0.38/proto/src/prost/v0_38/tendermint.abci.rs#L72-L85
     */
use cosmwasm_std::{Binary, Timestamp};

use super::consensus::ConsensusParams;
use super::validator::ValidatorUpdate;

#[derive(Debug, Clone, PartialEq)]
pub struct InitChainRequest {
    pub time: Timestamp,
    // FIXME: add network_id as well, like for avalanche?
    pub chain_id: String,
    pub consensus_params: ConsensusParams,
    pub validators: Vec<ValidatorUpdate>,
    pub app_state: Binary,
    pub initial_height: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InitChainResponse {
    pub consensus_params: ConsensusParams,
    pub validators: Vec<ValidatorUpdate>,
    pub app_hash: Vec<u8>,
}
