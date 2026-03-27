use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::metadata::ParseLevelError;
use tracing_subscriber::EnvFilter;

/// Raw user input for the global Slay3r config
#[derive(Serialize, Deserialize, Debug)]
pub struct RawConfig {
    /// The server we listen on (generally 127.0.0.1 or 0.0.0.0)
    pub host: String,

    /// The port we listen on
    pub port: u16,

    /// The log level we use
    pub log: String,

    pub read_buf_size: u32,

    // jeager collector to send trace data, if defined
    pub jaeger: Option<String>,

    // A directory to store the Rocks database (if missing use memory db)
    pub rocksdb: Option<String>,

    pub grpc: String,

    // Required
    pub rpc_url: Option<String>,
    // /// The directory we read all files from (default $HOME/.slay3r)
    // pub basedir: String,
}

impl Default for RawConfig {
    fn default() -> Self {
        RawConfig {
            host: "127.0.0.1".to_string(),
            port: 26658,
            log: "info".to_string(),
            read_buf_size: 4 * 1024 * 1024,
            jaeger: None,
            rocksdb: None,
            grpc: "0.0.0.0:9090".to_string(),
            rpc_url: None,
        }
    }
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Using a system port below 1024")]
    ReservedPort,

    #[error("You didn't provide a valid rpc_url for cometbft")]
    MissingRpcUrl,

    #[error("Unknown log level: {0}")]
    InvalidLogLevel(#[from] ParseLevelError),
}

impl RawConfig {
    /// Validate the config and return a type-safe version if okay
    pub fn validate(self) -> Result<Config, ConfigError> {
        if self.port < 1024 {
            return Err(ConfigError::ReservedPort);
        }
        let filter = EnvFilter::new(&self.log);
        // TODO: validate host, grpc, rpc_url
        // TODO: check if rocksdb path exists
        Ok(Config {
            host: self.host,
            port: self.port,
            filter,
            read_buf_size: self.read_buf_size,
            jaeger: self.jaeger,
            rocksdb: self.rocksdb,
            grpc: self.grpc,
            rpc_url: self.rpc_url.ok_or(ConfigError::MissingRpcUrl)?,
        })
    }
}

/// The global configuration for Slay3r
#[derive(Debug)]
pub struct Config {
    /// The server we listen on (generally 127.0.0.1 or 0.0.0.0)
    pub host: String,

    /// The port we listen on
    pub port: u16,

    /// The log level we use
    pub filter: EnvFilter,

    pub read_buf_size: u32,

    pub jaeger: Option<String>,

    // A directory to store the rocksdb database (if missing use memory db)
    pub rocksdb: Option<String>,

    /// Where to listen for gRPC connections (eg. 0.0.0.0:9090)
    pub grpc: String,

    /// Connection back to local cometbft rpc server, to relay some grpc requests (eg. http://cometbft:26657)
    pub rpc_url: String,
    // /// The directory we read all files from (default $HOME/.slay3r)
    // pub basedir: String,
}

/// The global configuration for Slay3r
#[derive(Debug)]
pub struct ServerData {
    pub server_port: String,
    pub grpc: String,
    pub rpc_url: String,
    pub has_jaeger: bool,
}

impl Config {
    pub fn extract_data(&self) -> ServerData {
        let server_port = format!("{}:{}", self.host, self.port);
        ServerData {
            server_port,
            grpc: self.grpc.clone(),
            rpc_url: self.rpc_url.clone(),
            has_jaeger: self.jaeger.is_some(),
        }
    }
}
