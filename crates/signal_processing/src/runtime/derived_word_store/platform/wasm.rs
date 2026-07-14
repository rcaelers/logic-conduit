//! Wasm in-memory derived-word storage.

#[path = "../store_wasm.rs"]
pub(crate) mod store;

pub use store::{IndexedAnnotationStore, IndexedAnnotationWriter, StoreResult};
