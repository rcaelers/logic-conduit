//! Compact, indexed storage for decoded [`Word`](crate::runtime::Word) streams.
//!
//! This module currently contains the versioned block format and its codec.
//! File lifecycle, live publication, and viewer queries are layered on top in
//! later implementation steps.

mod backend;
mod config;
#[cfg(test)]
mod contract_tests;
mod platform;
mod presence;
mod query;
mod state;

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        mod cache;
        pub(crate) mod codec;
        mod crc32c;
        mod format;
        mod persistent;
        mod vlq;
    }
}

pub(crate) use platform::store;

pub(crate) use backend::{AnnotationStoreBackend, AnnotationStoreWriterBackend};
pub use config::{BlockCodecConfig, LiveStoreConfig, PersistentStoreConfig};
pub use platform::*;
pub use query::{
    AnnotationQuery, AnnotationQueryError, AnnotationQueryResult, AnnotationStoreMetadata,
    ExactAnnotationWindow, WordPresenceBucket,
};
pub(crate) use state::LiveStoreMetadata;
pub use state::StoreStatus;

/// Errors caused by malformed input or a word stream that cannot be encoded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("word block is empty")]
    EmptyBlock,

    #[error(
        "word timestamp at index {index} moved backwards from {previous_timestamp_ns} ns to {timestamp_ns} ns"
    )]
    OutOfOrder {
        index: usize,
        previous_timestamp_ns: u64,
        timestamp_ns: u64,
    },

    #[error("restart interval must be greater than zero")]
    InvalidRestartInterval,

    #[error("invalid block codec configuration: {0}")]
    InvalidConfiguration(&'static str),

    #[error("word block contains too many records: {0}")]
    TooManyWords(usize),

    #[error("truncated derived-word block")]
    Truncated,

    #[error("unsigned VLQ exceeds 64 bits")]
    VlqOverflow,

    #[error("invalid derived-word format: {0}")]
    InvalidFormat(String),

    #[error("derived-word block checksum mismatch: expected {expected:#010x}, got {actual:#010x}")]
    ChecksumMismatch { expected: u32, actual: u32 },
}

pub type CodecResult<T> = std::result::Result<T, CodecError>;
