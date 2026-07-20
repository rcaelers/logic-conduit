mod builder;
mod definition;
mod implementation;
mod live_capture;
mod trigger;

pub(crate) use builder::DemoCaptureSourceBuilder;
pub use definition::{DemoCaptureSource, DemoCaptureSourceState};
pub use implementation::CapturePreviewSignal;
pub(crate) use implementation::{apply_live_capture_edit, capture_preview};
