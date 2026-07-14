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
pub mod logic;
pub mod sinks;
mod uart_demo_source;

pub use uart_demo_source::UartDemoSource;

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        mod dsl_file;
        mod dslogic_u3pro16;
        mod logic_analyzer;
        mod sigrok_file;

        pub use dsl_file::{
            DeferredDslFileSource, DslCaptureReader, DslChunkedCaptureReader,
            DslFileCaptureDataSource, DslFileSource,
        };
        pub use dslogic_u3pro16::{DsLogicU3Pro16, LinkSpeed, RusbTransport, UsbTransport};
        pub use logic_analyzer::{
            CaptureMode, ClockEdge, ClockSource, LogicAnalyzer, LogicAnalyzerError,
            LogicAnalyzerInfo, LogicAnalyzerResult, LogicAnalyzerSource, LogicCaptureConfig,
            LogicChunk, LogicEncoding, LogicEncodingRequest, LogicTrigger, LogicTriggerStage,
            TriggerCondition, TriggerLogic, DsLogicU3Pro16Source,
        };
        pub use sigrok_file::{
            SigrokCaptureReader, SigrokChunkedCaptureReader, SigrokFileCaptureDataSource,
            SigrokFileSource,
        };
    }
}

// Re-export Sample from runtime
pub use crate::runtime::sample::Sample;
