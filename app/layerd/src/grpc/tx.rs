use futures::future::try_join_all;
use std::sync::Arc;

use cosmwasm_std::Binary;
use layer_proto::cosmos::tx::v1beta1::{
    service_server::{Service, ServiceServer},
    BroadcastTxRequest, BroadcastTxResponse, GetBlockWithTxsRequest, GetBlockWithTxsResponse,
    GetTxRequest, GetTxResponse, GetTxsEventRequest, GetTxsEventResponse, SimulateRequest,
    SimulateResponse, TxDecodeAminoRequest, TxDecodeAminoResponse, TxDecodeRequest,
    TxDecodeResponse, TxEncodeAminoRequest, TxEncodeAminoResponse, TxEncodeRequest,
    TxEncodeResponse,
};
use layer_std::HexEncode;
use serde::Deserialize;

use layer_abci::MultiThreadedDispatcher;
use tendermint_rpc::{Client, HttpClient};
use tonic::{Request, Response, Status};

use super::{
    abci_response_to_grpc, grpc_request_to_abci,
    tendermint::{convert_block_id, convert_tendermint_block},
};

pub fn tx_service(
    dispatcher: Arc<MultiThreadedDispatcher>,
    rpc_url: &str,
) -> ServiceServer<TxService> {
    ServiceServer::new(TxService::new(dispatcher, rpc_url))
}

pub struct TxService {
    client: HttpClient,
    dispatcher: Arc<MultiThreadedDispatcher>,
}

impl TxService {
    pub fn new(dispatcher: Arc<MultiThreadedDispatcher>, rpc_url: &str) -> Self {
        Self {
            client: HttpClient::new(rpc_url).unwrap(),
            dispatcher,
        }
    }
}

pub(crate) fn invalid_arg(e: impl std::fmt::Display) -> Status {
    Status::new(tonic::Code::InvalidArgument, e.to_string())
}

pub(crate) fn gateway_error(e: tendermint_rpc::Error) -> Status {
    Status::new(tonic::Code::Internal, e.to_string())
}

impl TxService {
    async fn get_blocktime(
        &self,
        height: impl Into<tendermint::block::Height>,
    ) -> Result<String, Status> {
        let header = self
            .client
            .header(height.into())
            .await
            .map_err(gateway_error)?;
        Ok(header.header.time.to_rfc3339())
    }
}

#[tonic::async_trait]
impl Service for TxService {
    /// Simulate simulates executing a transaction for estimating gas usage.
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn simulate(
        &self,
        request: Request<SimulateRequest>,
    ) -> std::result::Result<tonic::Response<SimulateResponse>, Status> {
        let query = grpc_request_to_abci("/cosmos.tx.v1beta1.Service/Simulate", request.get_ref());
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc(response).map(Response::new)
    }

    /// GetTx fetches a tx by hash.
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn get_tx(
        &self,
        request: Request<GetTxRequest>,
    ) -> std::result::Result<tonic::Response<GetTxResponse>, Status> {
        // parse hex string into vec
        let binary = hex::decode(&request.get_ref().hash).map_err(invalid_arg)?;
        let hash = tendermint::hash::Hash::try_from(binary).map_err(invalid_arg)?;
        // This returns an error if hash not found
        // We now hardcode the error to "not found" rather than "internal" for compatibility with client tools.
        // I am very unsure how to differentiate the two based on the Tendermint RPC error enum.
        let tx = self
            .client
            .tx(hash, false)
            .await
            .map_err(|e| Status::new(tonic::Code::NotFound, e.to_string()))?;
        let time = self.get_blocktime(tx.height).await?;

        let response = GetTxResponse {
            tx: parse_cosmos_tx(&tx.tx),
            tx_response: Some(convert_tx_response(tx, time)),
        };
        Ok(tonic::Response::new(response))
    }

    /// BroadcastTx broadcast transaction.
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn broadcast_tx(
        &self,
        request: Request<BroadcastTxRequest>,
    ) -> std::result::Result<tonic::Response<BroadcastTxResponse>, Status> {
        let tx_bytes = request.get_ref().tx_bytes.as_slice();
        let response = match request.get_ref().mode {
            2 => {
                // BroadcastMode::Sync
                let tx = self
                    .client
                    .broadcast_tx_sync(tx_bytes)
                    .await
                    .map_err(gateway_error)?;
                BroadcastTxResponse {
                    tx_response: Some(convert_tx_broadcast_response(
                        tx.codespace,
                        tx.code,
                        tx.data,
                        tx.log,
                        tx.hash,
                    )),
                }
            }
            3 => {
                // BroadcastMode::Async
                let tx = self
                    .client
                    .broadcast_tx_async(tx_bytes)
                    .await
                    .map_err(gateway_error)?;
                BroadcastTxResponse {
                    tx_response: Some(convert_tx_broadcast_response(
                        tx.codespace,
                        tx.code,
                        tx.data,
                        tx.log,
                        tx.hash,
                    )),
                }
            }
            _ => {
                return Err(Status::new(
                    tonic::Code::InvalidArgument,
                    "Invalid broadcast mode",
                ));
            }
        };
        Ok(tonic::Response::new(response))
    }

    /// GetTxsEvent fetches txs by event.
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn get_txs_event(
        &self,
        request: Request<GetTxsEventRequest>,
    ) -> std::result::Result<tonic::Response<GetTxsEventResponse>, Status> {
        let r = request.get_ref();
        if r.events.len() != 1 {
            return Err(invalid_arg("only support exactly one event query for now"));
        }
        let query = r.events[0].parse().map_err(invalid_arg)?;
        let order = match r.order_by {
            1 => tendermint_rpc::Order::Ascending,
            2 => tendermint_rpc::Order::Descending,
            _ => return Err(invalid_arg("invalid order")),
        };

        // Try to handle older queries and never ones as well
        #[allow(deprecated)]
        let limit = if let Some(p) = r.pagination.as_ref() {
            p.limit
        } else {
            r.limit
        };
        let page = if r.page == 0 { 1 } else { r.page };

        let result = self
            .client
            .tx_search(query, false, page as u32, limit as u8, order)
            .await
            .map_err(gateway_error)?;

        let txs = result
            .txs
            .iter()
            .filter_map(|tx| parse_cosmos_tx(&tx.tx))
            .collect();
        let tx_responses: Result<Vec<_>, Status> =
            try_join_all(result.txs.into_iter().map(|tx| async {
                let time = self.get_blocktime(tx.height).await?;
                Ok(convert_tx_response(tx, time))
            }))
            .await;
        #[allow(deprecated)]
        let res = GetTxsEventResponse {
            txs,
            tx_responses: tx_responses?,
            pagination: None,
            total: result.total_count as u64,
        };
        Ok(tonic::Response::new(res))
    }

    /// GetBlockWithTxs fetches a block with decoded txs.
    ///
    /// Since: cosmos-sdk 0.45.2
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn get_block_with_txs(
        &self,
        request: Request<GetBlockWithTxsRequest>,
    ) -> std::result::Result<tonic::Response<GetBlockWithTxsResponse>, Status> {
        let block = self
            .client
            .block(request.get_ref().height as u32)
            .await
            .map_err(gateway_error)?;
        let block_data = convert_tendermint_block(&block.block);
        let txs = block
            .block
            .data
            .iter()
            .filter_map(|a| parse_cosmos_tx(a.as_slice()))
            .collect();
        let response = GetBlockWithTxsResponse {
            txs,
            block_id: Some(convert_block_id(&block.block_id)),
            block: Some(block_data),
            pagination: None,
        };
        Ok(tonic::Response::new(response))
    }

    /// TxDecode decodes the transaction.
    ///
    /// Since: cosmos-sdk 0.47
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn tx_decode(
        &self,
        _request: Request<TxDecodeRequest>,
    ) -> std::result::Result<tonic::Response<TxDecodeResponse>, Status> {
        Err(not_gonna_do_it("tx_decode"))
    }

    /// TxEncode encodes the transaction.
    ///
    /// Since: cosmos-sdk 0.47
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn tx_encode(
        &self,
        _request: Request<TxEncodeRequest>,
    ) -> std::result::Result<tonic::Response<TxEncodeResponse>, Status> {
        Err(not_gonna_do_it("tx_encode"))
    }

    /// TxEncodeAmino encodes an Amino transaction from JSON to encoded bytes.
    ///
    /// Since: cosmos-sdk 0.47
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn tx_encode_amino(
        &self,
        _request: Request<TxEncodeAminoRequest>,
    ) -> std::result::Result<tonic::Response<TxEncodeAminoResponse>, Status> {
        Err(not_gonna_do_it("tx_encode_amino"))
    }

    /// TxDecodeAmino decodes an Amino transaction from encoded bytes to JSON.
    ///
    /// Since: cosmos-sdk 0.47
    #[allow(clippy::blocks_in_conditions)]
    #[tracing::instrument(skip(self), level = "info", err(Debug))]
    async fn tx_decode_amino(
        &self,
        _request: Request<TxDecodeAminoRequest>,
    ) -> std::result::Result<tonic::Response<TxDecodeAminoResponse>, Status> {
        Err(not_gonna_do_it("tx_decode_amino"))
    }
}

fn not_gonna_do_it(msg: &str) -> Status {
    Status::new(
        tonic::Code::Unimplemented,
        format!("We are not going to do it: {}", msg),
    )
}

// Reads the responses from blocks
fn convert_tx_response(
    tx: tendermint_rpc::endpoint::tx::Response,
    timestamp: String, // This  must be queried separately from tendermint rpc
) -> layer_proto::cosmos::base::abci::v1beta1::TxResponse {
    let exec_tx = tx.tx_result;

    let _span = tracing::info_span!(
        "convert_tx_response",
        height = u64::from(tx.height),
        tx_hash = %HexEncode::new(&tx.hash.as_bytes()),
        raw_tx = %HexEncode::new(&tx.tx),
    )
    .entered();

    let logs = parse_log_structs(&exec_tx.log);
    layer_proto::cosmos::base::abci::v1beta1::TxResponse {
        height: tx.height.into(),
        txhash: hex::encode(tx.hash),
        codespace: exec_tx.codespace,
        code: exec_tx.code.into(),
        data: Binary(exec_tx.data.into()).to_base64(),
        raw_log: exec_tx.log,
        logs,
        info: exec_tx.info,
        gas_wanted: exec_tx.gas_wanted,
        gas_used: exec_tx.gas_used,
        tx: Some(layer_proto::google::protobuf::Any {
            // TODO: what type_url is this supposed to be?? Any????
            type_url: "/cosmos.tx.v1beta1.Tx".to_string(),
            value: tx.tx,
        }),
        timestamp,
        events: exec_tx.events.into_iter().map(convert_event).collect(),
    }
}

fn convert_event(event: tendermint::abci::Event) -> layer_proto::tendermint::abci::Event {
    layer_proto::tendermint::abci::Event {
        r#type: event.kind,
        attributes: event
            .attributes
            .iter()
            .map(|attr| layer_proto::tendermint::abci::EventAttribute {
                key: attr.key_str().unwrap().to_string(),
                value: attr.value_str().unwrap().to_string(),
                index: attr.index(),
            })
            .collect(),
    }
}

// Response from broadcast sync call
// Return None only if empty
fn convert_tx_broadcast_response(
    codespace: String,
    code: tendermint::abci::Code,
    data: bytes::Bytes,
    log: String,
    hash: tendermint::hash::Hash,
) -> layer_proto::cosmos::base::abci::v1beta1::TxResponse {
    layer_proto::cosmos::base::abci::v1beta1::TxResponse {
        height: 0,
        // hex encoding
        txhash: hex::encode(hash),
        codespace,
        code: code.into(),
        // base64 encoding
        data: Binary(data.into()).to_base64(),
        raw_log: log,
        logs: vec![],
        info: "".to_string(),
        gas_wanted: 0,
        gas_used: 0,
        tx: None,
        timestamp: "".to_string(),
        events: vec![],
    }
}

// STUPID STUFF CUZ REMOTE TYPES DON'T SUPPORT SERDE

/// ABCIMessageLog defines a structure containing an indexed tx ABCI message log.
#[derive(Clone, Debug, Deserialize)]
pub struct AbciMessageLog {
    pub msg_index: u32,
    pub log: String,
    /// Events contains a slice of Event objects that were emitted during some
    /// execution.
    pub events: Vec<StringEvent>,
}
/// StringEvent defines en Event object wrapper where all the attributes
/// contain key/value pairs that are strings instead of raw bytes.
#[derive(Clone, Debug, Deserialize)]
pub struct StringEvent {
    #[serde(rename = "type")]
    pub r#type: String,
    pub attributes: Vec<Attribute>,
}
/// Attribute defines an attribute wrapper where the key and value are
/// strings instead of raw bytes.
#[derive(Clone, Debug, Deserialize)]
pub struct Attribute {
    pub key: String,
    pub value: String,
}

fn parse_log_structs(log: &str) -> Vec<layer_proto::cosmos::base::abci::v1beta1::AbciMessageLog> {
    let ours: Vec<AbciMessageLog> = serde_json::from_str(log).unwrap_or_else(|_| vec![]);
    ours.into_iter()
        .map(
            |log| layer_proto::cosmos::base::abci::v1beta1::AbciMessageLog {
                msg_index: log.msg_index,
                log: log.log,
                events: log
                    .events
                    .into_iter()
                    .map(
                        |event| layer_proto::cosmos::base::abci::v1beta1::StringEvent {
                            r#type: event.r#type,
                            attributes: event
                                .attributes
                                .into_iter()
                                .map(|attr| layer_proto::cosmos::base::abci::v1beta1::Attribute {
                                    key: attr.key,
                                    value: attr.value,
                                })
                                .collect(),
                        },
                    )
                    .collect(),
            },
        )
        .collect()
}

// Again, cosmrs and different proto types...

fn parse_cosmos_tx(bytes: &[u8]) -> Option<layer_proto::cosmos::tx::v1beta1::Tx> {
    let tx = cosmrs::Tx::from_bytes(bytes).ok()?;
    let res = layer_proto::cosmos::tx::v1beta1::Tx {
        body: Some(layer_proto::cosmos::tx::v1beta1::TxBody {
            messages: tx.body.messages.into_iter().map(any_to_any).collect(),
            memo: tx.body.memo,
            timeout_height: tx.body.timeout_height.into(),
            extension_options: vec![],
            non_critical_extension_options: vec![],
        }),
        auth_info: Some(layer_proto::cosmos::tx::v1beta1::AuthInfo {
            signer_infos: tx
                .auth_info
                .signer_infos
                .into_iter()
                .map(signer_to_signer)
                .collect(),
            fee: Some(layer_proto::cosmos::tx::v1beta1::Fee {
                amount: tx.auth_info.fee.amount.iter().map(coin_to_coin).collect(),
                gas_limit: tx.auth_info.fee.gas_limit,
                payer: tx
                    .auth_info
                    .fee
                    .payer
                    .map(|x| x.to_string())
                    .unwrap_or("".to_string()),
                granter: tx
                    .auth_info
                    .fee
                    .granter
                    .map(|x| x.to_string())
                    .unwrap_or("".to_string()),
            }),
            tip: None,
        }),
        signatures: tx.signatures,
    };
    Some(res)
}

fn any_to_any(any: cosmrs::Any) -> layer_proto::google::protobuf::Any {
    layer_proto::google::protobuf::Any {
        type_url: any.type_url,
        value: any.value,
    }
}

fn coin_to_coin(coin: &cosmrs::Coin) -> layer_proto::cosmos::base::v1beta1::Coin {
    layer_proto::cosmos::base::v1beta1::Coin {
        denom: coin.denom.to_string(),
        amount: coin.amount.to_string(),
    }
}

fn signer_to_signer(
    signer: cosmrs::tx::SignerInfo,
) -> layer_proto::cosmos::tx::v1beta1::SignerInfo {
    let single = match signer.mode_info {
        cosmrs::tx::ModeInfo::Single(s) => layer_proto::cosmos::tx::v1beta1::mode_info::Single {
            mode: s.mode.into(),
        },
        // Safe to panic as this is tx we stored, we would have rejected anything else
        _ => panic!("Only single mode supported"),
    };

    let mi = layer_proto::cosmos::tx::v1beta1::ModeInfo {
        sum: Some(layer_proto::cosmos::tx::v1beta1::mode_info::Sum::Single(
            single,
        )),
    };

    layer_proto::cosmos::tx::v1beta1::SignerInfo {
        public_key: signer.public_key.map(|s| any_to_any(s.into())),
        mode_info: Some(mi),
        sequence: signer.sequence,
    }
}
