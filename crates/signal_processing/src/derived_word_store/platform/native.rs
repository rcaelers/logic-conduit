//! Native file-backed and mmap-backed derived-word storage.

pub use super::super::cache::{
    DecodedBlockCacheStats, configure_decoded_block_cache, decoded_block_cache_stats,
    reset_decoded_block_cache_stats,
};
pub use super::super::persistent::{
    cleanup_cache, clear_cache, clear_cache_entry, default_cache_directory,
};
#[path = "../store.rs"]
pub(crate) mod store;

pub use store::{
    CommittedAnnotationBlock, IndexedAnnotationStore, IndexedAnnotationWriter, StoreResult,
};
