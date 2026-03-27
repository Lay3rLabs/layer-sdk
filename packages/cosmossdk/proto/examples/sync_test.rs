use tonic::transport::Channel;

use layer_proto::layer::sync::v1::query_client::QueryClient as SyncClient;
use layer_proto::layer::sync::v1::{
    QueryLatestSequenceRequest, StreamChangesSinceRequest, StreamCurrentStateRequest,
};

const GRPC_ENDPOINT: &str = "http://localhost:9090";

#[tokio::main]
async fn main() {
    let channel = Channel::builder(GRPC_ENDPOINT.parse().unwrap())
        .connect()
        .await
        .unwrap();

    let mut client = SyncClient::new(channel);

    let request = tonic::Request::new(QueryLatestSequenceRequest {});
    let response = client.latest_sequence(request).await.unwrap();
    let sequence = response.get_ref().sequence;
    println!("Latest sequence = {:?}", sequence);

    current_state(&mut client).await;

    changes_since(&mut client, sequence).await;
}

async fn current_state(client: &mut SyncClient<Channel>) {
    let request = tonic::Request::new(StreamCurrentStateRequest {});
    let mut response = client.current_state(request).await.unwrap().into_inner();

    println!("**** CURRENT STATE *****\n");
    while let Some(state) = response.message().await.unwrap() {
        println!("{}", state);
    }
}

async fn changes_since(client: &mut SyncClient<Channel>, sequence: u64) {
    let request = tonic::Request::new(StreamChangesSinceRequest { sequence });
    let mut response = client.changes_since(request).await.unwrap().into_inner();

    println!("\n**** UPDATES *****\n");
    while let Some(block) = response.message().await.unwrap() {
        println!("{}", block);
    }
}
