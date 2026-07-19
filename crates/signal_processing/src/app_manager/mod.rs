//! Platform-neutral application runtime manager facade.

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

pub use imp::AppManager;
