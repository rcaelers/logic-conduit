//! Streaming signal-processing runtime, capture sources, and protocol decoders.
//!
//! This library provides a memory-efficient node runtime for processing captured and live
//! signals. Concrete adapters include DSLogic `.dsl` and Sigrok files and supported live
//! logic-analyzer hardware.
//!
//! # Architecture
//!
//! - **Capture sources**: Stream samples from files or live hardware
//! - **Streaming Nodes**: Thread-per-node execution with crossbeam channels
//! - **Scheduler**: Manages node lifecycle and parallel execution
//! - **Decoders**: SPI and parallel bus protocol decoders
//!
//! # Example
//!
//! ```no_run
//! use signal_processing::{DslFileSource, Pipeline};
//!
//! let mut pipeline = Pipeline::new();
//! pipeline.add_process("source", DslFileSource::new("capture.dsl", 12)?)?;
//! // ... connect nodes and run
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod nodes;
pub mod runtime;

// Re-export decoder data types
// Re-export streaming nodes - DslFileSource only (SpiCommandController is application-specific)
// Re-export streaming decoders
pub use nodes::UartDemoSource;
pub use nodes::decoders::{
    CsPolarity, ParallelDecoder, ParallelDecoderMetrics, ParallelDecoderMetricsSnapshot,
    ParallelInputStrategy, SpiDecoder, SpiMode, StrobeMode,
};
// Re-export control-path logic nodes and sinks
pub use nodes::logic::{
    BufferNode, GateOp, LogicGate, MatchOp, SrLatch, TextFormatter, TriggerAt, TriggerCounter,
    WordMatcher,
};
pub use nodes::sinks::{
    Annotation, AnnotationFold, BinaryFileWriter, CsvValueFormat, CsvWordWriter,
    DEFAULT_VIEWER_MAX_ENTRIES, DerivedLane, DerivedLaneData, DerivedLanes, DigitalFold,
    IndexedAnnotationLane, LaneSummary, MarkerFold, TextFileWriter, TgckRecorder, ViewerLaneKind,
    ViewerRetention, ViewerSink, ViewerSinkMetrics, ViewerSinkMetricsSnapshot, WriteWidth,
};
// Re-export data types from runtime
pub use runtime::derived_index::{AppendOnlyMipmap, ChunkedMipmap, LaneFold, MipmapRecord};
pub use runtime::derived_word_store::{
    AnnotationQuery, BlockCodecConfig, IndexedAnnotationStore, IndexedAnnotationWriter,
    LiveStoreConfig, PersistentStoreConfig, StoreStatus, WordPresenceBucket,
};
// Re-export streaming runtime components
pub use runtime::{
    AppManager, BlockCaptureSource, CaptureDataSource, CaptureFingerprint, CaptureIndex,
    CaptureMetadata, CaptureSampledChannel, CaptureSampledWindow, CaptureSource, CaptureTransition,
    CaptureWaveformSegment, ChannelMessage, ConfigOutcome, ConfigValue, Connection,
    ConnectionError, CooperativeManager, DisconnectEvent, DslHeader, DslSampledChannel,
    DslSampledWindow, DslTransition, DslWaveformSegment, EdgeQuery, GraphBuilder, InputPort,
    InputProtocolCandidate, InputSub, NodeConfig, NodeId, NodeSpec, NumberSample, OutputPort,
    OverflowPolicy, Pipeline, PortDirection, PortError, PortSchema, ProcessNode, ProtocolKind,
    Receiver, ReceiverSelector, Sample, SampleBlock, SampleKind, Scheduler, Sender, SharedSenders,
    StopHandle, TextSample, Trigger, Watchdog, Word, WorkError, WorkResult, register_type,
};

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        pub use nodes::{
            CaptureMode, ClockEdge, ClockSource, DeferredDslFileSource, DsLogicU3Pro16,
            DsLogicU3Pro16Source, DslCaptureReader, DslChunkedCaptureReader,
            DslFileCaptureDataSource, DslFileSource, LinkSpeed, LogicAnalyzer,
            LogicAnalyzerError, LogicAnalyzerInfo, LogicAnalyzerResult, LogicAnalyzerSource,
            LogicCaptureConfig, LogicChunk, LogicEncoding, LogicEncodingRequest, LogicTrigger,
            LogicTriggerStage, RusbTransport, SigrokCaptureReader, SigrokChunkedCaptureReader,
            SigrokFileCaptureDataSource, SigrokFileSource, TriggerCondition, TriggerLogic,
            UsbTransport,
        };
        pub use runtime::derived_word_store::{
            DecodedBlockCacheStats, cleanup_cache, clear_cache, clear_cache_entry,
            configure_decoded_block_cache, decoded_block_cache_stats, default_cache_directory,
            reset_decoded_block_cache_stats,
        };
        pub use runtime::{CaptureIndexProgress, IndexSampler, exact_window_sample_limit};
    }
}

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
