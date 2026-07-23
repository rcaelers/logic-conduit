//! Application-level coordination for immediate live capture.

#[cfg(test)]
mod architecture_tests;
mod implementation;

#[cfg(not(target_arch = "wasm32"))]
#[path = "native.rs"]
mod platform;
#[cfg(target_arch = "wasm32")]
#[path = "wasm.rs"]
mod platform;

pub(crate) use implementation::{
    CaptureAnalysisAttachment, CaptureAvailability, CaptureCoordinatorContract,
    CaptureReplayAttachment, ConfigurationEpochResolution, capture_availability,
};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use logic_analyzer_capture_export::CaptureExportFormat as CaptureRawExportFormat;
#[cfg(target_arch = "wasm32")]
pub(crate) use platform::CaptureCoordinator;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use platform::CaptureCoordinator;
