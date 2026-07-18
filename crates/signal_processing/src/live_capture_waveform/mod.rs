//! Incremental waveform queries over the authoritative live-capture store.

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        #[path = "native.rs"]
        mod native;

        pub use native::{NativeGrowingCaptureIndex, NativeGrowingCaptureIndexWorker};
    }
}
