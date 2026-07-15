//! Target selection for the derived-word-store implementation.
//!
//! The rest of the runtime compiles against the platform-neutral facade in
//! the parent module. Native filesystem/mmap behavior and wasm in-memory
//! behavior are selected here as complete implementation files.

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "wasm.rs"]
        mod imp;
    }
    _ => {
        #[path = "native.rs"]
        mod imp;
    }
}

pub(crate) use imp::store;
pub use imp::*;
