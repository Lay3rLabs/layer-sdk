use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use config::ServerData;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use http::{HeaderName, Method};
use layer_storage::PersistentStorage;
use tonic::transport::Server;
use tonic_web::GrpcWebLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;
use tracing_subscriber::fmt::time::LocalTime;
use tracing_subscriber::prelude::*;
use tracing_subscriber::FmtSubscriber;

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
    if let Some(collector) = config.jaeger.as_ref() {
        let endpoint = format!("{}/api/traces", collector);
        opentelemetry::global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
        let tracer = opentelemetry_jaeger::new_collector_pipeline()
            .with_endpoint(endpoint)
            //         // optionally set username and password as well.
            //         // .with_username("username")
            //         // .with_password("s3cr3t")
            .with_service_name("slay3rd")
            .with_isahc()
            .with_timeout(std::time::Duration::from_secs(2))
            .install_batch(opentelemetry::runtime::Tokio)
            .unwrap();
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

    let grpc_reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(include_bytes!(
            "../../../packages/cosmossdk/proto/src/protos/service_descriptor.bin"
        ))
        .build_v1()
        .unwrap();

    let grpc_server = Server::builder()
        .accept_http1(true)
        .layer(grpc::LogLayer::new("grpc"))
        .layer(cors_layer())
        .layer(GrpcWebLayer::new())
        .add_service(grpc_reflection)
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

// See: https://github.com/hyperium/tonic/issues/1524
fn cors_layer() -> CorsLayer {
    const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
    const DEFAULT_EXPOSED_HEADERS: [&str; 3] =
        ["grpc-status", "grpc-message", "grpc-status-details-bin"];
    const DEFAULT_ALLOW_HEADERS: [&str; 4] =
        ["x-grpc-web", "content-type", "x-user-agent", "grpc-timeout"];
    const DEFAULT_ALLOW_METHODS: [Method; 5] = [
        Method::POST,
        Method::GET,
        Method::OPTIONS,
        Method::PUT,
        Method::DELETE,
    ];

    CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_credentials(true)
        .max_age(DEFAULT_MAX_AGE)
        .expose_headers(
            DEFAULT_EXPOSED_HEADERS
                .iter()
                .cloned()
                .map(HeaderName::from_static)
                .collect::<Vec<HeaderName>>(),
        )
        .allow_headers(
            DEFAULT_ALLOW_HEADERS
                .iter()
                .cloned()
                .map(HeaderName::from_static)
                .collect::<Vec<HeaderName>>(),
        )
        .allow_methods(DEFAULT_ALLOW_METHODS)
}
