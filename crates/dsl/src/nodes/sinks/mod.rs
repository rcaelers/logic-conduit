//! Sink nodes — pipeline endpoints that persist or expose data

mod binary_file_writer;
mod csv_word_writer;
mod text_file_writer;
mod tgck_recorder;
mod viewer_sink;

pub use binary_file_writer::{BinaryFileWriter, WriteWidth};
pub use csv_word_writer::{CsvValueFormat, CsvWordWriter};
pub use text_file_writer::TextFileWriter;
pub use tgck_recorder::{TgckRecord, TgckRecorder};
#[cfg(not(target_arch = "wasm32"))]
pub use viewer_sink::IndexedAnnotationLane;
pub use viewer_sink::{
    AnnotationFold, DEFAULT_VIEWER_MAX_ENTRIES, DerivedLane, DerivedLaneData, DerivedLanes,
    DigitalFold, LaneSummary, MAX_ANNOTATION_NS, MarkerFold, ViewerLaneKind, ViewerRetention,
    ViewerSink, ViewerSinkMetrics, ViewerSinkMetricsSnapshot,
};

pub use crate::runtime::Annotation;
