use futures::stream::{Map, StreamExt};
use std::{ops::Deref, pin::Pin};
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

use layer_app::SyncProvider;
use layer_proto::layer::sync::v1::{
    query_server::{Query, QueryServer},
    BlockWrites, QueryLatestSequenceRequest, QueryLatestSequenceResponse,
    StreamChangesSinceRequest, StreamCurrentStateRequest, WriteData,
};
use layer_storage::PersistentStorage;

use crate::app::Pulsarium;

pub fn sync_service<T: PersistentStorage + 'static + Send + Sync>(
    app: Pulsarium<T>,
) -> QueryServer<SyncService<T>> {
    QueryServer::new(SyncService::new(app))
}

pub struct SyncService<T: PersistentStorage + 'static + Send + Sync> {
    app: Pulsarium<T>,
}

impl<T: PersistentStorage + 'static + Send + Sync> SyncService<T> {
    pub fn new(app: Pulsarium<T>) -> Self {
        Self { app }
    }
}

pub type MapType<T> = fn(Result<T, String>) -> Result<T, Status>;

pub fn internal_err<T>(x: Result<T, String>) -> Result<T, Status> {
    x.map_err(Status::internal)
}

pub type SyncStream<T> = Map<Pin<Box<dyn Stream<Item = Result<T, String>> + Send>>, MapType<T>>;

#[tonic::async_trait]
impl<T: PersistentStorage + 'static + Send + Sync> Query for SyncService<T> {
    type ChangesSinceStream = SyncStream<BlockWrites>;
    type CurrentStateStream = SyncStream<WriteData>;

    #[tracing::instrument(skip(self), level = "info")]
    async fn latest_sequence(
        &self,
        _request: Request<QueryLatestSequenceRequest>,
    ) -> Result<Response<QueryLatestSequenceResponse>, Status> {
        let lock = self.app.sync();
        let sequence = lock.deref().latest_sequence();
        Ok(Response::new(QueryLatestSequenceResponse { sequence }))
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn current_state(
        &self,
        _request: Request<StreamCurrentStateRequest>,
    ) -> Result<Response<Self::CurrentStateStream>, Status> {
        let lock = self.app.sync();
        let stream = lock.deref().current_state();
        Ok(Response::new(stream.map(internal_err)))
    }

    #[tracing::instrument(skip(self), level = "info")]
    async fn changes_since(
        &self,
        request: Request<StreamChangesSinceRequest>,
    ) -> Result<Response<Self::ChangesSinceStream>, Status> {
        let lock = self.app.sync();
        let stream = lock.deref().changes_since(request.get_ref().sequence);
        Ok(Response::new(stream.map(internal_err)))
    }
}
