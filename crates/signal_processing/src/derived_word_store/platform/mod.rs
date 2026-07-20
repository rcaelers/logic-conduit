//! Target selection for the derived-word-store implementation.
//!
//! The rest of the runtime compiles against the platform-neutral facade in
//! the parent module. Native filesystem/mmap behavior and wasm in-memory
//! behavior are selected here as complete implementation files.

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::default_working_directory;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{
    CommittedAnnotationBlock, DecodedBlockCacheStats, IndexedAnnotationStore,
    IndexedAnnotationWriter, StoreError, StoreResult, cleanup_cache, clear_cache,
    clear_cache_entry, configure_decoded_block_cache, decoded_block_cache_stats,
    reset_decoded_block_cache_stats,
};
#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::default_working_directory;
#[cfg(target_arch = "wasm32")]
pub use wasm::{IndexedAnnotationStore, IndexedAnnotationWriter, StoreError, StoreResult};
