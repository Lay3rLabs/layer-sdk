// use std::sync::Arc;

use std::sync::Arc;

use layer_abci::MultiThreadedDispatcher;
use layer_proto::cosmos::base::tendermint::v1beta1::VersionInfo;
use layer_proto::tendermint::p2p::{DefaultNodeInfo, DefaultNodeInfoOther, ProtocolVersion};
use layer_proto::tendermint::types::{BlockId, Header, PartSetHeader};
use layer_proto::{
    cosmos::base::tendermint::v1beta1::{
        service_server::{Service, ServiceServer},
        AbciQueryRequest, AbciQueryResponse, GetBlockByHeightRequest, GetBlockByHeightResponse,
        GetLatestBlockRequest, GetLatestBlockResponse, GetLatestValidatorSetRequest,
        GetLatestValidatorSetResponse, GetNodeInfoRequest, GetNodeInfoResponse, GetSyncingRequest,
        GetSyncingResponse, GetValidatorSetByHeightRequest, GetValidatorSetByHeightResponse,
    },
    tendermint::types::Block,
};

use tendermint::node::info::TxIndexStatus;
use tendermint_rpc::{Client, HttpClient, Paging};

use crate::grpc::{abci_response_to_grpc, grpc_request_to_abci};

use super::tx::gateway_error;

pub fn tendermint_service(
    dispatcher: Arc<MultiThreadedDispatcher>,
    rpc_url: &str,
) -> ServiceServer<TendermintService> {
    ServiceServer::new(TendermintService::new(dispatcher, rpc_url))
}

pub struct TendermintService {
    client: HttpClient,
    dispatcher: Arc<MultiThreadedDispatcher>,
}

impl TendermintService {
    pub fn new(dispatcher: Arc<MultiThreadedDispatcher>, rpc_url: &str) -> Self {
        Self {
            client: HttpClient::new(rpc_url).unwrap(),
            dispatcher,
        }
    }

    // shared between two calls
    async fn validator_set_by_height(
        &self,
        height: i64,
        pagination: Paging,
    ) -> std::result::Result<GetValidatorSetByHeightResponse, tonic::Status> {
        let vres = self
            .client
            .validators(height as u32, pagination)
            .await
            .map_err(gateway_error)?;

        let validators = vres
            .validators
            .into_iter()
            .map(
                |v| layer_proto::cosmos::base::tendermint::v1beta1::Validator {
                    address: v.address.to_string(),
                    pub_key: pub_key_to_any(v.pub_key),
                    voting_power: v.power.into(),
                    proposer_priority: v.proposer_priority.into(),
                },
            )
            .collect();

        let res = GetValidatorSetByHeightResponse {
            block_height: vres.block_height.into(),
            validators,
            pagination: None, // TODO: only vres.total is provided... not full pagination info
        };
        Ok(res)
    }
}

#[tonic::async_trait]
impl Service for TendermintService {
    /// GetNodeInfo queries the current node info.
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn get_node_info(
        &self,
        _request: tonic::Request<GetNodeInfoRequest>,
    ) -> std::result::Result<tonic::Response<GetNodeInfoResponse>, tonic::Status> {
        let status = self.client.status().await.map_err(gateway_error)?;
        let tx_index = match status.node_info.other.tx_index {
            TxIndexStatus::On => "on",
            TxIndexStatus::Off => "off",
        };
        let node_info = DefaultNodeInfo {
            protocol_version: Some(ProtocolVersion {
                p2p: status.node_info.protocol_version.p2p,
                block: status.node_info.protocol_version.block,
                app: status.node_info.protocol_version.app,
            }),
            default_node_id: status.node_info.id.to_string(),
            listen_addr: status.node_info.listen_addr.to_string(),
            network: status.node_info.network.to_string(),
            version: status.node_info.version.to_string(),
            channels: status.node_info.channels.to_string().into_bytes(),
            moniker: status.node_info.moniker.to_string(),
            other: Some(DefaultNodeInfoOther {
                tx_index: tx_index.to_string(),
                rpc_address: status.node_info.other.rpc_address,
            }),
        };
        let version_info = VersionInfo {
            name: "Slay3r".to_string(),
            app_name: "Slay3r".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_commit: env!("GIT_HASH").to_string(),
            build_tags: "".to_string(), // FIXME: read features
            go_version: env!("RUST_VERSION").to_string(),
            build_deps: vec![], // FIXME: what deps to put here?
            // Pretend we are 0.50.0 for now
            cosmos_sdk_version: "0.50.0".to_string(),
        };
        let response = GetNodeInfoResponse {
            default_node_info: Some(node_info),
            application_version: Some(version_info),
        };
        Ok(tonic::Response::new(response))
    }

    /// GetSyncing queries node syncing.
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn get_syncing(
        &self,
        _request: tonic::Request<GetSyncingRequest>,
    ) -> std::result::Result<tonic::Response<GetSyncingResponse>, tonic::Status> {
        let status = self.client.status().await.map_err(gateway_error)?;
        let response = GetSyncingResponse {
            syncing: status.sync_info.catching_up,
        };
        Ok(tonic::Response::new(response))
    }

    /// GetLatestBlock returns the latest block.
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn get_latest_block(
        &self,
        _request: tonic::Request<GetLatestBlockRequest>,
    ) -> std::result::Result<tonic::Response<GetLatestBlockResponse>, tonic::Status> {
        let block = self.client.latest_block().await.map_err(gateway_error)?;
        let block_data = convert_tendermint_block(&block.block);
        let response = GetLatestBlockResponse {
            block_id: Some(convert_block_id(&block.block_id)),
            block: Some(block_data),
            sdk_block: None,
        };
        Ok(tonic::Response::new(response))
    }

    /// GetBlockByHeight queries block for given height.
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn get_block_by_height(
        &self,
        request: tonic::Request<GetBlockByHeightRequest>,
    ) -> std::result::Result<tonic::Response<GetBlockByHeightResponse>, tonic::Status> {
        let block = self
            .client
            .block(request.get_ref().height as u32)
            .await
            .map_err(gateway_error)?;
        let block_data = convert_tendermint_block(&block.block);
        let response = GetBlockByHeightResponse {
            block_id: Some(convert_block_id(&block.block_id)),
            block: Some(block_data),
            sdk_block: None,
        };
        Ok(tonic::Response::new(response))
    }

    /// GetLatestValidatorSet queries latest validator-set.
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn get_latest_validator_set(
        &self,
        _request: tonic::Request<GetLatestValidatorSetRequest>,
    ) -> std::result::Result<tonic::Response<GetLatestValidatorSetResponse>, tonic::Status> {
        // Query the block height, then call the version by height.
        // tendermint_rpc doesn't have a separate call for this
        let height = self
            .client
            .status()
            .await
            .map_err(gateway_error)?
            .sync_info
            .latest_block_height;

        // Do the same height query
        // let pagination = request.get_ref().pagination;
        let pagination = Paging::Default; // TODO: implement pagination
        let vres = self
            .validator_set_by_height(height.into(), pagination)
            .await?;

        // and then copy over the data into a different named, but same shaped struct
        let res = GetLatestValidatorSetResponse {
            block_height: vres.block_height,
            validators: vres.validators.clone(),
            pagination: vres.pagination,
        };
        Ok(tonic::Response::new(res))
    }

    /// GetValidatorSetByHeight queries validator-set at a given height.
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn get_validator_set_by_height(
        &self,
        request: tonic::Request<GetValidatorSetByHeightRequest>,
    ) -> std::result::Result<tonic::Response<GetValidatorSetByHeightResponse>, tonic::Status> {
        let height = request.get_ref().height;
        // let pagination = request.get_ref().pagination;
        let pagination = Paging::Default; // TODO: implement pagination
        let res = self.validator_set_by_height(height, pagination).await?;
        Ok(tonic::Response::new(res))
    }

    /// ABCIQuery defines a query handler that supports ABCI queries directly to the
    /// application, bypassing Tendermint completely. The ABCI query must contain
    /// a valid and supported path, including app, custom, p2p, and store.
    ///
    /// Since: cosmos-sdk 0.46
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn abci_query(
        &self,
        request: tonic::Request<AbciQueryRequest>,
    ) -> std::result::Result<tonic::Response<AbciQueryResponse>, tonic::Status> {
        let query = grpc_request_to_abci(&request.get_ref().path, &request.get_ref().data);
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc(response).map(tonic::Response::new)
    }
}

pub(crate) fn convert_tendermint_block(
    b: &tendermint::block::Block,
) -> layer_proto::tendermint::types::Block {
    let evidence = if b.evidence.as_ref().is_empty() {
        None
    } else {
        Some(layer_proto::tendermint::types::EvidenceList {
            evidence: b.evidence.iter().map(convert_evidence).collect(),
        })
    };

    Block {
        header: Some(convert_tendermint_header(&b.header)),
        data: Some(layer_proto::tendermint::types::Data {
            txs: b.data.clone(),
        }),
        evidence,
        last_commit: b.last_commit.as_ref().map(convert_tendermint_commit),
    }
}

fn time_to_timestamp(t: tendermint::Time) -> layer_proto::google::protobuf::Timestamp {
    let t2: tendermint_proto::google::protobuf::Timestamp = t.into();
    layer_proto::google::protobuf::Timestamp {
        seconds: t2.seconds,
        nanos: t2.nanos,
    }
}

fn vote_to_vote(v: &tendermint::Vote) -> layer_proto::tendermint::types::Vote {
    layer_proto::tendermint::types::Vote {
        r#type: v.vote_type.into(),
        height: v.height.into(),
        round: v.round.into(),
        block_id: v.block_id.map(|a| convert_block_id(&a)),
        timestamp: v.timestamp.map(time_to_timestamp),
        validator_address: v.validator_address.into(),
        validator_index: v.validator_index.into(),
        signature: maybe_sig_to_bytes(v.signature.as_ref()),
    }
}

fn maybe_sig_to_bytes(s: Option<&tendermint::signature::Signature>) -> Vec<u8> {
    s.map(|s| s.as_bytes().to_vec()).unwrap_or_default()
}

fn commit_sig_to_sig(
    s: &tendermint::block::CommitSig,
) -> layer_proto::tendermint::types::CommitSig {
    match s {
        tendermint::block::CommitSig::BlockIdFlagAbsent => {
            layer_proto::tendermint::types::CommitSig {
                block_id_flag: layer_proto::tendermint::types::BlockIdFlag::Absent as i32,
                validator_address: vec![],
                timestamp: None,
                signature: vec![],
            }
        }
        tendermint::block::CommitSig::BlockIdFlagCommit {
            validator_address,
            timestamp,
            signature,
        } => layer_proto::tendermint::types::CommitSig {
            block_id_flag: layer_proto::tendermint::types::BlockIdFlag::Commit as i32,
            validator_address: (*validator_address).into(),
            timestamp: Some(time_to_timestamp(*timestamp)),
            signature: maybe_sig_to_bytes(signature.as_ref()),
        },
        tendermint::block::CommitSig::BlockIdFlagNil {
            validator_address,
            timestamp,
            signature,
        } => layer_proto::tendermint::types::CommitSig {
            block_id_flag: layer_proto::tendermint::types::BlockIdFlag::Nil as i32,
            validator_address: (*validator_address).into(),
            timestamp: Some(time_to_timestamp(*timestamp)),
            signature: maybe_sig_to_bytes(signature.as_ref()),
        },
    }
}

fn convert_evidence(
    ev: &tendermint::evidence::Evidence,
) -> layer_proto::tendermint::types::Evidence {
    let sum = match ev {
        tendermint::evidence::Evidence::DuplicateVote(e) => {
            layer_proto::tendermint::types::evidence::Sum::DuplicateVoteEvidence(
                layer_proto::tendermint::types::DuplicateVoteEvidence {
                    vote_a: Some(vote_to_vote(&e.vote_a)),
                    vote_b: Some(vote_to_vote(&e.vote_b)),
                    total_voting_power: e.total_voting_power.into(),
                    validator_power: e.validator_power.into(),
                    timestamp: Some(time_to_timestamp(e.timestamp)),
                },
            )
        }
        tendermint::evidence::Evidence::LightClientAttack(_e) => {
            layer_proto::tendermint::types::evidence::Sum::LightClientAttackEvidence(
                #[allow(unreachable_code)]
                layer_proto::tendermint::types::LightClientAttackEvidence {
                    conflicting_block: todo!(),
                    common_height: todo!(),
                    byzantine_validators: todo!(),
                    total_voting_power: todo!(),
                    timestamp: todo!(),
                },
            )
        }
    };
    layer_proto::tendermint::types::Evidence { sum: Some(sum) }
}

fn convert_tendermint_header(
    h: &tendermint::block::Header,
) -> layer_proto::tendermint::types::Header {
    let block_time: tendermint_proto::google::protobuf::Timestamp = h.time.into();
    Header {
        version: Some(layer_proto::tendermint::version::Consensus {
            block: h.version.block,
            app: h.version.app,
        }),
        chain_id: h.chain_id.to_string(),
        height: u64::from(h.height) as i64,
        time: Some(layer_proto::google::protobuf::Timestamp {
            seconds: block_time.seconds,
            nanos: block_time.nanos,
        }),
        last_block_id: h.last_block_id.as_ref().map(convert_block_id),
        last_commit_hash: opt_hash_to_vec(h.last_commit_hash),
        data_hash: opt_hash_to_vec(h.data_hash),
        validators_hash: hash_to_vec(h.validators_hash),
        next_validators_hash: hash_to_vec(h.next_validators_hash),
        consensus_hash: hash_to_vec(h.consensus_hash),
        app_hash: h.app_hash.as_bytes().into(),
        last_results_hash: opt_hash_to_vec(h.last_results_hash),
        evidence_hash: opt_hash_to_vec(h.evidence_hash),
        proposer_address: h.proposer_address.into(),
    }
}

fn convert_tendermint_commit(
    c: &tendermint::block::Commit,
) -> layer_proto::tendermint::types::Commit {
    layer_proto::tendermint::types::Commit {
        height: u64::from(c.height) as i64,
        round: c.round.into(),
        block_id: Some(convert_block_id(&c.block_id)),
        signatures: c.signatures.iter().map(commit_sig_to_sig).collect(),
    }
}

pub(crate) fn convert_block_id(id: &tendermint::block::Id) -> BlockId {
    BlockId {
        hash: hash_to_vec(id.hash),
        part_set_header: Some(PartSetHeader {
            total: id.part_set_header.total,
            hash: hash_to_vec(id.part_set_header.hash),
        }),
    }
}

fn opt_hash_to_vec(h: Option<tendermint::Hash>) -> Vec<u8> {
    match h {
        None => vec![],
        Some(v) => hash_to_vec(v),
    }
}

fn hash_to_vec(h: tendermint::Hash) -> Vec<u8> {
    match h {
        tendermint::Hash::None => vec![],
        tendermint::Hash::Sha256(v) => v.to_vec(),
    }
}

fn pub_key_to_any(p: tendermint::PublicKey) -> Option<layer_proto::google::protobuf::Any> {
    let (type_url, value) = match p {
        // TODO: verify which types we use... the same file defines two (for JSON and for Protobuf...)
        tendermint::PublicKey::Ed25519(pk) => (
            "tendermint.crypto.PublicKey_Ed25519".to_string(),
            // "tendermint/PubKeyEd25519".to_string(),
            pk.as_bytes().to_vec(),
        ),
        // hidden under a feature flag
        // tendermint::PublicKey::Secp256k1(pk) => (
        //     "tendermint/PubKeySecp256k1".to_string(),
        //     pk.as_bytes().to_vec(),
        // ),
        _ => return None,
    };
    Some(layer_proto::google::protobuf::Any { type_url, value })
}
