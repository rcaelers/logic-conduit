//! Runtime support for streaming node graphs

pub mod capture;
pub mod cooperative_manager;
pub mod derived_index;
pub mod edge_query;
pub mod errors;
pub mod events;
pub mod graph;
pub mod manager;
pub mod node;
pub mod pipeline;
pub mod ports;
pub mod protocol;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod raw_block_cache;
pub mod receiver;
pub mod sample;
pub mod scheduler;
pub mod sender;
pub mod type_registry;
pub mod watchdog;
#[cfg(not(target_arch = "wasm32"))]
pub mod waveform_index;

pub use capture::{
    BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureIndex,
    CaptureMetadata, CaptureSampledChannel, CaptureSampledWindow, CaptureSource, CaptureTransition,
    CaptureWaveformSegment, DslHeader, DslSampledChannel, DslSampledWindow, DslTransition,
    DslWaveformSegment, packed_bit,
};
pub use cooperative_manager::CooperativeManager;
pub use edge_query::EdgeQuery;
pub use errors::{ConnectionError, PortError, WorkError, WorkResult};
pub use events::{NumberSample, TextSample, Trigger};
pub use graph::{Connection, GraphBuilder, NodeId};
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
pub use node::{ConfigOutcome, ConfigValue, NodeConfig, ProcessNode};
pub use ports::{InputPort, OutputPort, Pipeline, PortDirection, PortSchema, register_type};
pub use protocol::ProtocolKind;
pub use receiver::{Receiver, ReceiverSelector};
pub use sample::Sample;
pub use sample::SampleBlock;
pub use scheduler::{Scheduler, StopHandle};
pub use sender::ChannelMessage;
pub use sender::{OverflowPolicy, Sender, SharedSenders};
pub use watchdog::Watchdog;
#[cfg(not(target_arch = "wasm32"))]
pub use waveform_index::{CaptureIndexProgress, IndexSampler, exact_window_sample_limit};
