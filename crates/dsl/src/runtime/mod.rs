//! Runtime support for streaming node graphs

pub(crate) mod capture;
pub(crate) mod cooperative_manager;
pub(crate) mod derived_index;
pub mod derived_word_store;
pub(crate) mod edge_query;
pub(crate) mod errors;
pub(crate) mod events;
pub(crate) mod graph;
#[cfg_attr(target_arch = "wasm32", path = "idle_wasm.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "idle_native.rs")]
mod idle;
pub(crate) mod manager;
pub(crate) mod node;
pub(crate) mod pipeline;
pub(crate) mod ports;
pub(crate) mod protocol;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod raw_block_cache;
pub(crate) mod receiver;
pub(crate) mod sample;
pub(crate) mod sample_kind;
pub(crate) mod scheduler;
pub(crate) mod sender;
pub(crate) mod type_registry;
pub(crate) mod watchdog;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod waveform_index;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod worker_pool;

pub use capture::{
    BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureIndex,
    CaptureMetadata, CaptureSampledChannel, CaptureSampledWindow, CaptureSource, CaptureTransition,
    CaptureWaveformSegment, DslHeader, DslSampledChannel, DslSampledWindow, DslTransition,
    DslWaveformSegment, packed_bit,
};
pub use cooperative_manager::CooperativeManager;
pub use edge_query::EdgeQuery;
pub use errors::{ConnectionError, PortError, WorkError, WorkResult};
pub use events::{
    Annotation, MAX_ANNOTATION_NS, NumberSample, TextSample, Trigger, Word,
    instantaneous_word_end_ns,
};
pub use graph::{Connection, GraphBuilder, NodeId};
pub(crate) use idle::idle_backoff;
pub use manager::{DisconnectEvent, InputSub, NodeSpec, PipelineManager};

/// The pipeline supervisor the app layer builds live graphs on: real OS
/// threads natively, a cooperative single-thread runner on wasm (no
/// `std::thread` there). Both expose the same surface — see
/// [`manager::PipelineManager`] and [`cooperative_manager::CooperativeManager`]
/// — so callers never branch on target.
#[cfg(not(target_arch = "wasm32"))]
pub type AppManager = PipelineManager;
#[cfg(target_arch = "wasm32")]
pub type AppManager = CooperativeManager;
pub use node::{ConfigOutcome, ConfigValue, InputProtocolCandidate, NodeConfig, ProcessNode};
pub use pipeline::Pipeline;
pub use ports::{InputPort, OutputPort, PortDirection, PortSchema, register_type};
pub use protocol::ProtocolKind;
pub use receiver::{Receiver, ReceiverSelector};
pub use sample::{Sample, SampleBlock};
pub use sample_kind::SampleKind;
pub use scheduler::{Scheduler, StopHandle};
pub use sender::{ChannelMessage, OverflowPolicy, Sender, SharedSenders};
pub use watchdog::Watchdog;
#[cfg(not(target_arch = "wasm32"))]
pub use waveform_index::{CaptureIndexProgress, IndexSampler, exact_window_sample_limit};
