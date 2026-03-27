use std::sync::Arc;

use layer_proto::cosmwasm::wasm::v1::{
    query_server::{Query, QueryServer},
    QueryAllContractStateRequest, QueryAllContractStateResponse, QueryCodeRequest,
    QueryCodeResponse, QueryCodesRequest, QueryCodesResponse, QueryContractHistoryRequest,
    QueryContractHistoryResponse, QueryContractInfoRequest, QueryContractInfoResponse,
    QueryContractsByCodeRequest, QueryContractsByCodeResponse, QueryParamsRequest,
    QueryParamsResponse, QueryPinnedCodesRequest, QueryPinnedCodesResponse,
    QueryRawContractStateRequest, QueryRawContractStateResponse, QuerySmartContractStateRequest,
    QuerySmartContractStateResponse,
};

use layer_abci::MultiThreadedDispatcher;
use tonic::{Request, Response, Status};

use super::{abci_response_to_grpc, grpc_request_to_abci, unimplemented};

pub fn cosmwasm_service(dispatcher: Arc<MultiThreadedDispatcher>) -> QueryServer<CosmWasmService> {
    QueryServer::new(CosmWasmService::new(dispatcher))
}

pub struct CosmWasmService {
    dispatcher: Arc<MultiThreadedDispatcher>,
}

impl CosmWasmService {
    pub fn new(dispatcher: Arc<MultiThreadedDispatcher>) -> Self {
        Self { dispatcher }
    }
}

#[tonic::async_trait]
impl Query for CosmWasmService {
    /// ContractInfo gets the contract meta data
    #[tracing::instrument(skip(self), level = "info")]
    async fn contract_info(
        &self,
        request: Request<QueryContractInfoRequest>,
    ) -> Result<Response<QueryContractInfoResponse>, Status> {
        let query = grpc_request_to_abci("/cosmwasm.wasm.v1.Query/ContractInfo", request.get_ref());
        let response = self.dispatcher.dispatch_query(query).await;
        let mut res: QueryContractInfoResponse = abci_response_to_grpc(response)?;
        res.address = request.get_ref().address.clone();
        Ok(Response::new(res))
    }

    /// ContractHistory gets the contract code history
    #[tracing::instrument(skip(self), level = "info")]
    async fn contract_history(
        &self,
        _request: Request<QueryContractHistoryRequest>,
    ) -> Result<Response<QueryContractHistoryResponse>, Status> {
        todo!();
    }
    /// ContractsByCode lists all smart contracts for a code id
    #[tracing::instrument(skip(self), level = "info")]
    async fn contracts_by_code(
        &self,
        request: Request<QueryContractsByCodeRequest>,
    ) -> Result<Response<QueryContractsByCodeResponse>, Status> {
        let query =
            grpc_request_to_abci("/cosmwasm.wasm.v1.Query/ContractsByCode", request.get_ref());
        let response = self.dispatcher.dispatch_query(query).await;
        let res: QueryContractsByCodeResponse = abci_response_to_grpc(response)?;
        // TODO: pagination info
        Ok(Response::new(res))
    }
    /// AllContractState gets all raw store data for a single contract
    #[tracing::instrument(skip(self), level = "info")]
    async fn all_contract_state(
        &self,
        _request: Request<QueryAllContractStateRequest>,
    ) -> Result<Response<QueryAllContractStateResponse>, Status> {
        todo!();
    }

    /// RawContractState gets single key from the raw store data of a contract
    #[tracing::instrument(skip(self), level = "info")]
    async fn raw_contract_state(
        &self,
        request: Request<QueryRawContractStateRequest>,
    ) -> Result<Response<QueryRawContractStateResponse>, Status> {
        let query = grpc_request_to_abci(
            "/cosmwasm.wasm.v1.Query/RawContractState",
            request.get_ref(),
        );
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc(response).map(Response::new)
    }

    /// SmartContractState get smart query result from the contract
    #[tracing::instrument(skip(self), level = "info")]
    async fn smart_contract_state(
        &self,
        request: Request<QuerySmartContractStateRequest>,
    ) -> Result<Response<QuerySmartContractStateResponse>, Status> {
        let query = grpc_request_to_abci(
            "/cosmwasm.wasm.v1.Query/SmartContractState",
            request.get_ref(),
        );
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc(response).map(Response::new)
    }

    /// Code gets the binary code and metadata for a singe wasm code
    // #[tracing::instrument(skip(self), level = "info")]
    async fn code(
        &self,
        request: Request<QueryCodeRequest>,
    ) -> Result<Response<QueryCodeResponse>, Status> {
        let query = grpc_request_to_abci("/cosmwasm.wasm.v1.Query/Code", request.get_ref());
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc(response).map(Response::new)
    }

    /// Codes gets the metadata for all stored wasm codes
    #[tracing::instrument(skip(self), level = "info")]
    async fn codes(
        &self,
        request: Request<QueryCodesRequest>,
    ) -> Result<Response<QueryCodesResponse>, Status> {
        let query = grpc_request_to_abci("/cosmwasm.wasm.v1.Query/Codes", request.get_ref());
        let response = self.dispatcher.dispatch_query(query).await;
        let res: QueryCodesResponse = abci_response_to_grpc(response)?;
        // TODO: pagination info
        Ok(Response::new(res))
    }

    /// PinnedCodes gets the pinned code ids
    #[tracing::instrument(skip(self), level = "info")]
    async fn pinned_codes(
        &self,
        _request: Request<QueryPinnedCodesRequest>,
    ) -> Result<Response<QueryPinnedCodesResponse>, Status> {
        Err(unimplemented("pinned_codes")) // TODO
    }

    /// Params gets the module params
    #[tracing::instrument(skip(self), level = "info")]
    async fn params(
        &self,
        _request: Request<QueryParamsRequest>,
    ) -> Result<Response<QueryParamsResponse>, Status> {
        Err(unimplemented("params")) // TODO
    }
}
