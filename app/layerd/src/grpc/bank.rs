use std::sync::Arc;

use layer_abci::MultiThreadedDispatcher;
use layer_proto::cosmos::bank::v1beta1::{
    query_server::{Query, QueryServer},
    QueryAllBalancesRequest, QueryAllBalancesResponse, QueryBalanceRequest, QueryBalanceResponse,
    QueryDenomMetadataRequest, QueryDenomMetadataResponse, QueryDenomsMetadataRequest,
    QueryDenomsMetadataResponse, QueryParamsRequest, QueryParamsResponse,
    QuerySpendableBalancesRequest, QuerySpendableBalancesResponse, QuerySupplyOfRequest,
    QuerySupplyOfResponse, QueryTotalSupplyRequest, QueryTotalSupplyResponse,
};
// use ibc_proto::cosmos::base::v1beta1::Coin as RawCoin;
use tonic::{Request, Response, Status};

use super::{abci_response_to_grpc, grpc_request_to_abci, unimplemented};

pub fn bank_service(dispatcher: Arc<MultiThreadedDispatcher>) -> QueryServer<BankService> {
    QueryServer::new(BankService::new(dispatcher))
}

pub struct BankService {
    dispatcher: Arc<MultiThreadedDispatcher>,
}

impl BankService {
    pub fn new(dispatcher: Arc<MultiThreadedDispatcher>) -> Self {
        Self { dispatcher }
    }
}

#[tonic::async_trait]
impl Query for BankService {
    #[tracing::instrument(skip(self), level = "info")]
    async fn balance(
        &self,
        request: Request<QueryBalanceRequest>,
    ) -> Result<Response<QueryBalanceResponse>, Status> {
        let query = grpc_request_to_abci("/cosmos.bank.v1beta1.Query/Balance", request.get_ref());
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc(response).map(Response::new)
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn all_balances(
        &self,
        request: Request<QueryAllBalancesRequest>,
    ) -> Result<Response<QueryAllBalancesResponse>, Status> {
        let query =
            grpc_request_to_abci("/cosmos.bank.v1beta1.Query/AllBalances", request.get_ref());
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc(response).map(Response::new)
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn spendable_balances(
        &self,
        request: Request<QuerySpendableBalancesRequest>,
    ) -> Result<Response<QuerySpendableBalancesResponse>, Status> {
        let query = grpc_request_to_abci(
            "/cosmos.bank.v1beta1.Query/SpendableBalances",
            request.get_ref(),
        );
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc(response).map(Response::new)
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn total_supply(
        &self,
        request: Request<QueryTotalSupplyRequest>,
    ) -> Result<Response<QueryTotalSupplyResponse>, Status> {
        let query =
            grpc_request_to_abci("/cosmos.bank.v1beta1.Query/TotalSupply", request.get_ref());
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc(response).map(Response::new)
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn supply_of(
        &self,
        request: Request<QuerySupplyOfRequest>,
    ) -> Result<Response<QuerySupplyOfResponse>, Status> {
        let query = grpc_request_to_abci("/cosmos.bank.v1beta1.Query/SupplyOf", request.get_ref());
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc(response).map(Response::new)
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn params(
        &self,
        _request: Request<QueryParamsRequest>,
    ) -> Result<Response<QueryParamsResponse>, Status> {
        Err(unimplemented("params")) // TODO
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn denom_metadata(
        &self,
        _request: Request<QueryDenomMetadataRequest>,
    ) -> Result<Response<QueryDenomMetadataResponse>, Status> {
        Err(unimplemented("denom_metadata")) // TODO
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn denoms_metadata(
        &self,
        _request: Request<QueryDenomsMetadataRequest>,
    ) -> Result<Response<QueryDenomsMetadataResponse>, Status> {
        Err(unimplemented("denoms_metadata")) // TODO
    }
}
