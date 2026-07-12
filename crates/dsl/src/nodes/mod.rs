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

pub mod decoders;
#[cfg(not(target_arch = "wasm32"))]
mod dsl_file;
#[cfg(not(target_arch = "wasm32"))]
mod dslogic_u3pro16;
pub mod logic;
#[cfg(not(target_arch = "wasm32"))]
mod logic_analyzer;
#[cfg(not(target_arch = "wasm32"))]
mod sigrok_file;
pub mod sinks;
mod uart_demo_source;

// Export DslFileSource and related types for file I/O
#[cfg(not(target_arch = "wasm32"))]
pub use dsl_file::{
    DeferredDslFileSource, DslCaptureReader, DslChunkedCaptureReader, DslFileCaptureDataSource,
    DslFileSource,
};
#[cfg(not(target_arch = "wasm32"))]
pub use dslogic_u3pro16::{DsLogicU3Pro16, LinkSpeed, RusbTransport, UsbTransport};
#[cfg(not(target_arch = "wasm32"))]
pub use logic_analyzer::{
    CaptureMode, ClockEdge, ClockSource, LogicAnalyzer, LogicAnalyzerError, LogicAnalyzerInfo,
    LogicAnalyzerResult, LogicAnalyzerSource, LogicCaptureConfig, LogicChunk, LogicEncoding,
    LogicEncodingRequest, LogicTrigger, LogicTriggerStage, TriggerCondition, TriggerLogic,
};
#[cfg(not(target_arch = "wasm32"))]
pub use sigrok_file::SigrokFileSource;
pub use uart_demo_source::UartDemoSource;

/// Convenient name for the production DSLogic source node.
#[cfg(not(target_arch = "wasm32"))]
pub type DsLogicU3Pro16Source = LogicAnalyzerSource<DsLogicU3Pro16<RusbTransport>>;

// Re-export Sample from runtime
pub use crate::runtime::Sample;
