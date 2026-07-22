//! Generic immutable capture contracts and packed capture data.

#[cfg(not(target_arch = "wasm32"))]
#[path = "../capture_backing_native.rs"]
mod backing;
#[cfg(target_arch = "wasm32")]
#[path = "../capture_backing_wasm.rs"]
mod backing;
mod implementation;

pub use implementation::{
    BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureIndex,
    CaptureIndexBuildProgress, CaptureIndexFactory, CaptureMetadata, CaptureSampledChannel,
    CaptureSampledWindow, CaptureSource, CaptureTransition, CaptureWaveformSegment,
    IndexedCapturePresentation, packed_bit,
};
