#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

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
mod types;
mod viewer;
#[cfg(not(target_arch = "wasm32"))]
mod worker;

pub use lanes::{
    AnnotationVisual, DefaultViewerLaneRenderer, DerivedLaneId, ViewerLaneBadge, ViewerLaneFrame,
    ViewerLaneGroup, ViewerLaneGroupId, ViewerLaneRegistry, ViewerLaneRenderer, ViewerLaneTrack,
    ViewerLaneTrackFrame, ViewerLaneTrackId, ViewerOutputPresentation,
};
pub use viewer::{ChannelSignal, LogicAnalyzerViewer};
