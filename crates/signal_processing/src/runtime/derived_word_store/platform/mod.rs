//! Target selection for the derived-word store implementation.
//!
//! The rest of the runtime compiles against the platform-neutral facade in
//! the parent module. Native filesystem/mmap behavior and wasm in-memory
//! behavior are selected here as complete implementation files.

pub(super) use super::{CodecError, backend, config, presence, query, state};
#[cfg(not(target_arch = "wasm32"))]
pub(super) use super::{cache, codec, format, persistent};

#[cfg_attr(target_arch = "wasm32", path = "wasm.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "native.rs")]
mod imp;

pub(crate) use imp::store;
pub use imp::*;
