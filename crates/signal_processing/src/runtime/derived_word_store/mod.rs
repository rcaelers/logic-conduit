//! Compact, indexed storage for decoded [`Word`](crate::runtime::events::Word) streams.
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

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        mod cache;
        mod codec;
        mod crc32c;
        mod format;
        mod persistent;
        mod vlq;
    }
}

pub(crate) use backend::{AnnotationStoreBackend, AnnotationStoreWriterBackend};
pub use config::{BlockCodecConfig, LiveStoreConfig, PersistentStoreConfig};
pub use errors::{CodecError, CodecResult};
pub(crate) use platform::store;
pub use platform::*;
pub use query::{
    AnnotationQuery, AnnotationQueryError, AnnotationQueryResult, AnnotationStoreMetadata,
    ExactAnnotationWindow, WordPresenceBucket,
};
pub(crate) use state::LiveStoreMetadata;
pub use state::StoreStatus;
