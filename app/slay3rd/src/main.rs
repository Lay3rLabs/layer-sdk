use std::env;
use std::path::{Path, PathBuf};

use clap::Parser;
use config::ServerData;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use layer_storage::PersistentStorage;
use tonic::transport::Server;
use tracing::info;
use tracing_subscriber::fmt::time::LocalTime;
use tracing_subscriber::prelude::*;
use tracing_subscriber::FmtSubscriber;

use opentelemetry::{trace::TracerProvider, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{trace::Config, Resource};

use layer_abci::ServerConfig;
use layer_app::AppConfig;

mod app;
mod cli;
mod config;
mod convert;
mod decode;
mod encode;
mod grpc;

use crate::app::Pulsarium;
use crate::cli::Cli;
use crate::config::RawConfig;

/// We check for slay3r home dir:
/// * from --home flag
/// * from SLAY_HOME env var
/// * default to $HOME/.slay3r
fn get_home() -> PathBuf {
    let mut pargs = pico_args::Arguments::from_env();

    // check for --home flag
    if let Some(home) = pargs.opt_value_from_str::<_, String>("--home").unwrap() {
        return PathBuf::from(home);
    }

    // check SLAY_HOME
    if let Ok(home) = env::var("SLAY_HOME") {
        return PathBuf::from(home);
    }

    // default to $HOME/.slay3r
    Path::new(&env::var("HOME").unwrap()).join(".slay3r")
}

#[tokio::main]
async fn main() {
    let home = get_home();
    let config_file = home.as_path().join("config/slay3r.toml");
    println!("Reading config file from {}", config_file.to_str().unwrap());

    // Parse all config info
    let args = Cli::parse();
    // Thanks to https://steezeburger.com/2023/03/rust-hierarchical-configuration/ for this tip
    let config: RawConfig = Figment::from(Serialized::defaults(RawConfig::default()))
        .merge(Toml::file(config_file))
        .merge(Env::prefixed("SLAY_"))
        .merge(Serialized::defaults(args))
        .extract()
        .unwrap();
    // We print this out for debugging before the logger is set up
    println!("{:?}", config);
    let config = config.validate().unwrap();
    let data = config.extract_data();

    // add open telemetry
    if let Some(endpoint) = config.jaeger.as_ref() {
        let otlp_exporter = opentelemetry_otlp::new_exporter()
            .tonic()
            .with_endpoint(endpoint)
            .with_timeout(std::time::Duration::from_secs(2));
        let trace_cfg = Config::default().with_resource(Resource::new(vec![KeyValue::new(
            "service.name",
            "layer-sdk",
        )]));

        let provider = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(otlp_exporter)
            .with_trace_config(trace_cfg)
            .install_batch(opentelemetry_sdk::runtime::Tokio)
            // .install_simple()
            .unwrap();
        let tracer = provider.tracer("layer-sdk");

        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
        let subscriber = tracing_subscriber::Registry::default()
            .with(config.filter)
            .with(telemetry);

        // let subscriber = fmt_subscriber.with(telemetry);
        tracing::subscriber::set_global_default(subscriber)
            .expect("setting default subscriber failed");
        info!("jaeger tracing enabled");
    } else {
        let fmt_subscriber = FmtSubscriber::builder()
            .with_env_filter(config.filter)
            .with_timer(LocalTime::rfc_3339())
            .with_ansi(true)
            .finish();

        tracing::subscriber::set_global_default(fmt_subscriber)
            .expect("setting default subscriber failed");
    }

    // autogenerate wasm_dir from home
    let wasm_path = home.as_path().join("data");
    let wasm_dir = wasm_path.to_str().unwrap();
    let app_config = AppConfig::new(wasm_dir);

    // Create the app
    let server_config = ServerConfig::new().with_read_buf(config.read_buf_size as usize);

    // Create ABCI server
    match config.rocksdb {
        Some(path) => {
            info!("using rocks db at {}", path);
            let storage = layer_storage::RockStore::open(&path);
            let app = Pulsarium::new(storage, app_config);
            run_server(app, data, server_config).await
        }
        None => {
            info!("using in-memory database");
            let storage = layer_storage::MemoryStore::new();
            let app = Pulsarium::new(storage, app_config);
            run_server(app, data, server_config).await
        }
    };
}

async fn run_server<T: PersistentStorage + 'static + Send + Sync>(
    app: Pulsarium<T>,
    data: ServerData,
    server_config: ServerConfig,
) {
    let server = server_config
        .bind(data.server_port, app.clone())
        .await
        .unwrap();

    let query = server.query_dispatcher();
    let grpc_server = Server::builder()
        .layer(grpc::LogLayer::new("grpc"))
        .add_service(grpc::auth_service(query.clone()))
        .add_service(grpc::bank_service(query.clone()))
        .add_service(grpc::sync_service(app))
        .add_service(grpc::tx_service(query.clone(), &data.rpc_url))
        .add_service(grpc::tendermint_service(query.clone(), &data.rpc_url))
        .add_service(grpc::cosmwasm_service(query));

    let grpc_result =
        tokio::task::spawn(async move { grpc_server.serve(data.grpc.parse().unwrap()).await });

    // we run as long as the abci server is up.
    server.listen().await.unwrap();

    // kill async tasks (grpc server, jaeger agent) when main task is done
    grpc_result.abort();
    if data.has_jaeger {
        opentelemetry::global::shutdown_tracer_provider();
    }
}
