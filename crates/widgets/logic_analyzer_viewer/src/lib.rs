#[cfg(test)]
mod architecture_tests;
mod channel;
mod cursor;
mod draw;
mod format;
mod indexed_annotations;
mod input;
mod lanes;
mod sampling;
mod sampling_overlay;
mod simple_trigger;
mod types;
mod viewer;
std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "worker_wasm.rs"]
        mod worker;
    }
    _ => {
        mod worker;
    }
}

pub use lanes::{
    AnnotationVisual, DefaultViewerLaneRenderer, DerivedLaneId, ViewerLaneBadge, ViewerLaneFrame,
    ViewerLaneGroup, ViewerLaneGroupId, ViewerLaneRegistry, ViewerLaneRenderer, ViewerLaneTrack,
    ViewerLaneTrackFrame, ViewerLaneTrackId, ViewerOutputPresentation,
};
pub use sampling_overlay::{SamplingEdge, SamplingOverlay, SamplingQualifier};
pub use simple_trigger::{SimpleTriggerEdit, SimpleTriggerLane};
pub use types::{ColorProfile, ViewerRowId};
pub use viewer::{ChannelSignal, LogicAnalyzerViewer};
