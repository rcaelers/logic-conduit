//! Generic immutable capture contracts and packed capture data.

#[cfg(not(target_arch = "wasm32"))]
#[path = "../capture_backing_native.rs"]
mod backing;
#[cfg(target_arch = "wasm32")]
#[path = "../capture_backing_wasm.rs"]
mod backing;
mod implementation;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use implementation::BlockBacking;
pub use implementation::{
    BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureIndex,
    CaptureMetadata, CaptureSampledChannel, CaptureSampledWindow, CaptureSource, CaptureTransition,
    CaptureWaveformSegment, packed_bit,
};
