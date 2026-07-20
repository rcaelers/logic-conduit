//! Wasm in-memory derived-word storage.

#[path = "../../store_wasm.rs"]
mod store;

pub(crate) use store::default_working_directory;
pub use store::{IndexedAnnotationStore, IndexedAnnotationWriter, StoreError, StoreResult};
