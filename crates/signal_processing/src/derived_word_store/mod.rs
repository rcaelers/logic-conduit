//! Compact, indexed storage for decoded [`Word`](crate::events::Word) streams.
//!
//! This module currently contains the versioned block format and its codec.
//! File lifecycle, live publication, and viewer queries are layered on top in
//! later implementation steps.

mod backend;
mod config;
#[cfg(test)]
mod contract_tests;
mod errors;
mod platform;
mod presence;
mod query;
mod state;

#[cfg(not(target_arch = "wasm32"))]
mod cache;
#[cfg(not(target_arch = "wasm32"))]
mod codec;
#[cfg(not(target_arch = "wasm32"))]
mod format;
#[cfg(not(target_arch = "wasm32"))]
mod persistent;
#[cfg(not(target_arch = "wasm32"))]
mod vlq;

pub(crate) use backend::{AnnotationStoreBackend, AnnotationStoreWriterBackend};
pub use config::{BlockCodecConfig, LiveStoreConfig, PersistentStoreConfig};
pub use errors::{CodecError, CodecResult};
#[cfg(not(target_arch = "wasm32"))]
pub use platform::{
    CommittedAnnotationBlock, DecodedBlockCacheStats, cleanup_cache, clear_cache,
    clear_cache_entry, configure_decoded_block_cache, decoded_block_cache_stats,
    reset_decoded_block_cache_stats,
};
pub use platform::{IndexedAnnotationStore, IndexedAnnotationWriter, StoreError, StoreResult};
pub use query::{
    AnnotationQuery, AnnotationQueryError, AnnotationQueryResult, AnnotationStoreMetadata,
    ExactAnnotationWindow, WordPresenceBucket,
};
pub use state::{LiveStoreMetadata, StoreStatus};
