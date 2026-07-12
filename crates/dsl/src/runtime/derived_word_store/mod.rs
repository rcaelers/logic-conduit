//! Compact, indexed storage for decoded [`Word`](crate::runtime::Word) streams.
//!
//! This module currently contains the versioned block format and its codec.
//! File lifecycle, live publication, and viewer queries are layered on top in
//! later implementation steps.

#[cfg(not(target_arch = "wasm32"))]
mod cache;
mod codec;
mod crc32c;
mod format;
#[cfg(not(target_arch = "wasm32"))]
mod persistent;
mod presence;
mod query;
#[cfg(not(target_arch = "wasm32"))]
mod store;
mod vlq;

#[cfg(not(target_arch = "wasm32"))]
pub use cache::{
    DEFAULT_DECODED_BLOCK_CACHE_BYTES, DecodedBlockCacheStats, configure_decoded_block_cache,
    decoded_block_cache_stats, reset_decoded_block_cache_stats,
};
pub use codec::{
    BlockCodecConfig, DecodedWordBlock, DecodedWordRange, EncodedBlockMetadata, PushResult,
    WordBlockBuilder, decode_word_block, decode_word_block_range, encode_word_block,
    find_restart_for_timestamp,
};
pub use format::{
    BLOCK_FLAG_HAS_DURATIONS, BLOCK_HEADER_SIZE, BLOCK_MAGIC, BlockDirectoryEntry,
    DATA_HEADER_SIZE, DATA_MAGIC, DEFAULT_MAX_BLOCK_PAYLOAD_BYTES, DEFAULT_MAX_INTER_WORD_GAP_NS,
    DEFAULT_MAX_WORDS_PER_BLOCK, DEFAULT_RESTART_INTERVAL, DataFileHeader, FORMAT_VERSION,
    RestartEntry, WordBlockHeader,
};
#[cfg(not(target_arch = "wasm32"))]
pub use persistent::{PersistentCacheStats, cleanup_cache, clear_cache, default_cache_directory};
pub use presence::{WordPresenceIndex, WordSummaryRecord};
pub use query::{
    AnnotationQuery, AnnotationQueryError, AnnotationQueryResult, AnnotationStoreMetadata,
    ExactAnnotationWindow, WordPresenceBucket,
};
#[cfg(not(target_arch = "wasm32"))]
pub use store::{
    DEFAULT_HOT_TAIL_PUBLISH_INTERVAL, DEFAULT_HOT_TAIL_PUBLISH_WORDS,
    DEFAULT_MAX_PERSISTENT_CACHE_BYTES, IndexedAnnotationStore, IndexedAnnotationWriter,
    LiveStoreConfig, LiveStoreMetadata, LiveStoreSnapshot, PersistentStoreConfig, StoreError,
    StoreResult, StoreStatus,
};
pub use vlq::{decode_u64 as decode_vlq_u64, encode_u64 as encode_vlq_u64, encoded_len};

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
