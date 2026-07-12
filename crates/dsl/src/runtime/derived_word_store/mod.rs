//! Compact, indexed storage for decoded [`Word`](crate::runtime::Word) streams.
//!
//! This module currently contains the versioned block format and its codec.
//! File lifecycle, live publication, and viewer queries are layered on top in
//! later implementation steps.

mod backend;
#[cfg(not(target_arch = "wasm32"))]
mod cache;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod codec;
mod config;
#[cfg(test)]
mod contract_tests;
#[cfg(not(target_arch = "wasm32"))]
mod crc32c;
#[cfg(not(target_arch = "wasm32"))]
mod format;
#[cfg(not(target_arch = "wasm32"))]
mod persistent;
mod presence;
mod query;
mod state;
#[cfg_attr(target_arch = "wasm32", path = "store_wasm.rs")]
mod store;
#[cfg(not(target_arch = "wasm32"))]
mod vlq;

pub(crate) use backend::{AnnotationStoreBackend, AnnotationStoreWriterBackend};
#[cfg(not(target_arch = "wasm32"))]
pub use cache::{
    DecodedBlockCacheStats, configure_decoded_block_cache, decoded_block_cache_stats,
    reset_decoded_block_cache_stats,
};
pub use config::{BlockCodecConfig, LiveStoreConfig, PersistentStoreConfig};
#[cfg(not(target_arch = "wasm32"))]
pub use persistent::{cleanup_cache, clear_cache, clear_cache_entry, default_cache_directory};
pub use query::{
    AnnotationQuery, AnnotationQueryError, AnnotationQueryResult, AnnotationStoreMetadata,
    ExactAnnotationWindow, WordPresenceBucket,
};
pub(crate) use state::LiveStoreMetadata;
pub use state::StoreStatus;
pub use store::{IndexedAnnotationStore, IndexedAnnotationWriter, StoreResult};

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
