//! Wasm in-memory derived-word storage.

pub(super) use super::{CodecError, backend, config, presence, query, state};

#[path = "../store_wasm.rs"]
pub(crate) mod store;

pub use store::{IndexedAnnotationStore, IndexedAnnotationWriter, StoreResult};
