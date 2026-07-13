//! Native file-backed and mmap-backed derived-word storage.

pub use super::cache::{
    DecodedBlockCacheStats, configure_decoded_block_cache, decoded_block_cache_stats,
    reset_decoded_block_cache_stats,
};
pub use super::persistent::{
    cleanup_cache, clear_cache, clear_cache_entry, default_cache_directory,
};
pub(super) use super::{
    CodecError, backend, cache, codec, config, format, persistent, presence, query, state,
};

#[path = "../store.rs"]
pub(crate) mod store;

pub use store::{IndexedAnnotationStore, IndexedAnnotationWriter, StoreResult};
