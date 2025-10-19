use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(String),
    
    #[error("Protocol error: {0}")]
    Protocol(String),
    
    #[error("Routing error: {0}")]
    Routing(String),
    
    #[error("Proxy error: {0}")]
    Proxy(String),
    
    #[error("Cache error: {0}")]
    Cache(String),
    
    #[error("Telemetry error: {0}")]
    Telemetry(String),
    
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Unknown error: {0}")]
    Unknown(String),
}

pub type Result<T> = std::result::Result<T, Error>;