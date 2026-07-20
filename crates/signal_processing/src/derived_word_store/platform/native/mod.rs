//! Native file-backed and mmap-backed derived-word storage.

pub use super::super::cache::{
    DecodedBlockCacheStats, configure_decoded_block_cache, decoded_block_cache_stats,
    reset_decoded_block_cache_stats,
};
pub use super::super::persistent::{cleanup_cache, clear_cache, clear_cache_entry};
#[path = "../../store.rs"]
mod store;

pub(crate) use store::default_working_directory;
pub use store::{
    CommittedAnnotationBlock, IndexedAnnotationStore, IndexedAnnotationWriter, StoreError,
    StoreResult,
};
