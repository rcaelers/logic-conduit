//! DSLogic DSL file parser with streaming node-based API
//!
//! This library provides a memory-efficient, streaming API for reading DSLogic .dsl files
//! and processing captured data in real-time using a thread-per-node graph architecture.
//!
//! # Architecture
//!
//! - **DslFileSource**: Streams samples from DSL files with on-demand ZIP archive reads
//! - **Streaming Nodes**: Thread-per-node execution with crossbeam channels
//! - **Scheduler**: Manages node lifecycle and parallel execution
//! - **Decoders**: SPI and parallel bus protocol decoders
//!
//! # Example
//!
//! ```no_run
//! use dsl::{DslFileSource, Pipeline};
//!
//! let mut pipeline = Pipeline::new();
//! pipeline.add_process("source", DslFileSource::new("capture.dsl", 12)?)?;
//! // ... connect nodes and run
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod nodes;
pub mod runtime;

// Re-export decoder data types
pub use nodes::decoders::{CsPolarity, ParallelWord, SpiMode, SpiTransfer, StrobeMode, TimingInfo};

// Re-export data types from runtime
pub use runtime::Sample;
pub use runtime::SampleBlock;
pub use runtime::{NumberSample, TextSample, Trigger};

// Re-export streaming nodes - DslFileSource only (SpiCommandController is application-specific)
pub use nodes::UartDemoSource;
#[cfg(not(target_arch = "wasm32"))]
pub use nodes::{
    CaptureMode, ClockEdge, ClockSource, DsLogicU3Pro16, DsLogicU3Pro16Source, DslCaptureReader,
    DslChunkedCaptureReader, DslFileCaptureDataSource, DslFileSource, LinkSpeed, LogicAnalyzer,
    LogicAnalyzerError, LogicAnalyzerInfo, LogicAnalyzerResult, LogicAnalyzerSource,
    LogicCaptureConfig, LogicChunk, LogicEncoding, LogicEncodingRequest, LogicTrigger,
    LogicTriggerStage, RusbTransport, TriggerCondition, TriggerLogic, UsbTransport,
};

pub use runtime::{
    BlockCaptureSource, CaptureDataSource, CaptureFingerprint, CaptureIndex, CaptureMetadata,
    CaptureSampledChannel, CaptureSampledWindow, CaptureSource, CaptureTransition,
    CaptureWaveformSegment, DslHeader, DslSampledChannel, DslSampledWindow, DslTransition,
    DslWaveformSegment,
};
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::{CaptureIndexProgress, IndexSampler, exact_window_sample_limit};

// Re-export streaming decoders
pub use nodes::decoders::ParallelDecoder;
pub use nodes::decoders::SpiDecoder;

// Re-export control-path logic nodes and sinks
pub use nodes::logic::{
    BufferNode, GateOp, LogicGate, MatchOp, SrLatch, TextFormatter, TriggerCounter, WordField,
    WordMatcher, WordSource,
};
pub use nodes::sinks::{
    Annotation, AnnotationFold, BinaryFileWriter, DerivedLane, DerivedLaneData, DerivedLanes,
    DigitalFold, LaneSummary, MarkerFold, TextFileWriter, TgckRecorder, ViewerLaneKind, ViewerSink,
    WriteWidth,
};
pub use runtime::derived_index::{AppendOnlyMipmap, LaneFold, MipmapRecord};

// Re-export streaming runtime components
pub use runtime::{
    Connection, ConnectionError, GraphBuilder, InputPort, NodeId, OutputPort, Pipeline,
    PortDirection, PortError, PortSchema, ProcessNode, Scheduler, WorkError, WorkResult,
    register_type,
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("Header parsing error: {0}")]
    ParseHeader(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Missing required field: {0}")]
    MissingField(String),

    #[error("Invalid probe number: {0}")]
    InvalidProbe(usize),

    #[error("Invalid block number: {0}")]
    InvalidBlock(u64),

    #[error("Position out of bounds: {0}")]
    OutOfBounds(u64),
}

pub type DslError = Error;

pub type Result<T> = std::result::Result<T, Error>;
