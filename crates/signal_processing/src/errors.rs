//! Error types for the runtime system.

use std::any::TypeId;

use crossbeam_channel::{RecvError, SendError};

/// Errors returned by capture and indexed-signal infrastructure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("Header parsing error: {0}")]
    ParseHeader(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Missing required field: {0}")]
    MissingField(String),

    #[error("Invalid probe number: {0}")]
    InvalidProbe(usize),

    #[error("Invalid block number: {0}")]
    InvalidBlock(u64),

    #[error("Position out of bounds: {0}")]
    OutOfBounds(u64),
}

pub type DslError = Error;
pub type Result<T> = std::result::Result<T, Error>;

/// Error type for port operations
#[derive(Debug, thiserror::Error)]
pub enum PortError {
    #[error("Port '{0}' not found on node '{1}'")]
    NotFound(String, String),

    #[error("Port index {0} out of range for node '{1}'")]
    IndexOutOfRange(usize, String),
}

/// Error type for connection operations
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error(
        "Type mismatch: {from_node}.{from_port} ({from_type:?}) -> {to_node}.{to_port} ({to_type:?})"
    )]
    TypeMismatch {
        from_node: String,
        from_port: String,
        from_type: TypeId,
        to_node: String,
        to_port: String,
        to_type: TypeId,
    },

    #[error("Node '{0}' not found")]
    NodeNotFound(String),

    #[error("Port '{port}' not found on node '{node}'")]
    PortNotFound { node: String, port: String },

    #[error("{0}")]
    DuplicateConnection(String),
}

/// Error type for work function operations
#[derive(Debug, thiserror::Error)]
pub enum WorkError {
    #[error("Failed to receive from input channel: {0}")]
    RecvError(#[from] RecvError),

    #[error("Failed to send to output channel: {0}")]
    SendError(String),

    #[error("Node-specific error: {0}")]
    NodeError(String),

    #[error("Shutdown signal received")]
    Shutdown,
}

impl<T> From<SendError<T>> for WorkError {
    fn from(e: SendError<T>) -> Self {
        WorkError::SendError(format!("{}", e))
    }
}

/// Result type for work functions
pub type WorkResult<T = ()> = std::result::Result<T, WorkError>;
