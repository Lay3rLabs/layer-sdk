use std::sync::Arc;

use layer_proto::cosmos::auth::v1beta1::{
    query_server::{Query, QueryServer},
    QueryAccountRequest, QueryAccountResponse, QueryAccountsRequest, QueryAccountsResponse,
    QueryModuleAccountByNameRequest, QueryModuleAccountByNameResponse, QueryParamsRequest,
    QueryParamsResponse,
};

use layer_abci::MultiThreadedDispatcher;
use tonic::{Request, Response, Status};

use super::{abci_response_to_grpc, grpc_request_to_abci, unimplemented};

pub fn auth_service(dispatcher: Arc<MultiThreadedDispatcher>) -> QueryServer<AuthService> {
    QueryServer::new(AuthService::new(dispatcher))
}

pub struct AuthService {
    dispatcher: Arc<MultiThreadedDispatcher>,
}

impl AuthService {
    pub fn new(dispatcher: Arc<MultiThreadedDispatcher>) -> Self {
        Self { dispatcher }
    }
}

#[tonic::async_trait]
impl Query for AuthService {
    #[tracing::instrument(skip(self), level = "info")]
    async fn accounts(
        &self,
        _request: Request<QueryAccountsRequest>,
    ) -> Result<Response<QueryAccountsResponse>, Status> {
        Err(unimplemented("accounts")) // TODO
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn account(
        &self,
        request: Request<QueryAccountRequest>,
    ) -> Result<Response<QueryAccountResponse>, Status> {
        let query = grpc_request_to_abci("/cosmos.auth.v1beta1.Query/Account", request.get_ref());
        let response = self.dispatcher.dispatch_query(query).await;
        abci_response_to_grpc::<QueryAccountResponse>(response).map(Response::new)
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn params(
        &self,
        _request: Request<QueryParamsRequest>,
    ) -> Result<Response<QueryParamsResponse>, Status> {
        Err(unimplemented("params")) // TODO
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn module_account_by_name(
        &self,
        _request: Request<QueryModuleAccountByNameRequest>,
    ) -> Result<Response<QueryModuleAccountByNameResponse>, Status> {
        Err(unimplemented("module_account_by_name")) // TODO
    }
}
