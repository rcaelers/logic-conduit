//! Sink nodes — pipeline endpoints that persist or expose data

mod binary_file_writer;
mod viewer_sink;

pub use binary_file_writer::{BinaryFileWriter, WriteWidth};
pub use viewer_sink::{
    Annotation, DerivedLane, DerivedLaneData, DerivedLanes, MAX_ANNOTATION_NS, MAX_LANE_ENTRIES,
    ViewerLaneKind, ViewerSink,
};
