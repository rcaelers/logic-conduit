//! Platform-neutral authoritative live-capture storage.

mod implementation;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
mod repository_native;

pub use implementation::{
    CaptureCursorItem, CaptureReclamationReport, CaptureRecordingGate, CaptureRecoveryReport,
    CaptureSessionMetadata, CaptureSessionOutcome, CaptureStoreCursor, CaptureStoreDescriptor,
    CaptureStoreError, CaptureStoreManifest, CaptureStoreResult, CaptureStoreSnapshot,
    CaptureTimelineMetadata, RecordingCaptureCursor,
};
#[cfg(not(target_arch = "wasm32"))]
pub use native::{
    NativeCaptureCursor, NativeCaptureRandomReader, NativeCaptureStore, NativeCaptureStoreConfig,
    NativeCaptureStoreWriter, NativeFinalizedCapture,
};
#[cfg(not(target_arch = "wasm32"))]
pub use repository_native::{
    CaptureSessionCleanupPlan, NativeCaptureSessionPin, NativeCaptureSessionRepository,
    NativeCaptureSessionRepositoryConfig, NativeCaptureSessionSummary,
};
