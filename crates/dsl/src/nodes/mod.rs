//! Node-based signal processing system
//!
//! This module provides a streaming node graph system for real-time signal processing:
//! - **Nodes**: Computation units that process samples
//! - **Channels**: Crossbeam channels for inter-node communication
//! - **Scheduler**: Thread-per-node runtime for parallel execution
//! - **Decoders**: Protocol decoders (SPI, parallel bus)
//!
//! # Architecture
//!
//! The streaming architecture uses thread-per-node execution:
//! - Source nodes produce samples (files)
//! - Process nodes transform data (decoders)
//! - Sink nodes consume results (printers, analyzers)
//! - All connected via crossbeam MPSC channels
//!
//! # Examples
//!
//! ```ignore
//! use dsl_loader::{DslFileSource, Scheduler};
//!
//! let source = DslFileSource::new("capture.dsl", 12)?;
//! // ... set up channels and run with Scheduler
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod capture;
pub mod decoders;
mod dsl_file;
mod dsl_index;
mod dslogic_u3pro16;
mod logic_analyzer;

pub use capture::{
    BlockCaptureSource, CaptureActivity, CaptureFingerprint, CaptureMetadata,
    CaptureSampledChannel, CaptureSampledWindow, CaptureSource, CaptureSourceFactory,
    CaptureTransition, DslActivity, DslHeader, DslSampledChannel, DslSampledWindow, DslTransition,
};
// Export DslFileSource and related types for file I/O
pub use dsl_file::{DslCaptureReader, DslFileCaptureFactory, DslFileSource};
pub use dsl_index::{DslChunkedCaptureReader, DslIndexProgress, IndexedCaptureReader};
pub use dslogic_u3pro16::{DsLogicU3Pro16, LinkSpeed, RusbTransport, UsbTransport};
pub use logic_analyzer::{
    CaptureMode, ClockEdge, ClockSource, LogicAnalyzer, LogicAnalyzerError, LogicAnalyzerInfo,
    LogicAnalyzerResult, LogicAnalyzerSource, LogicCaptureConfig, LogicChunk, LogicEncoding,
    LogicEncodingRequest, LogicTrigger, LogicTriggerStage, TriggerCondition, TriggerLogic,
};

/// Convenient name for the production DSLogic source node.
pub type DsLogicU3Pro16Source = LogicAnalyzerSource<DsLogicU3Pro16<RusbTransport>>;

// Re-export Sample from runtime
pub use crate::runtime::Sample;
