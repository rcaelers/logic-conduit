//! Runtime support for streaming node graphs

pub mod capture;
pub mod errors;
pub mod events;
pub mod graph;
pub mod manager;
pub mod node;
pub mod pipeline;
pub mod ports;
pub(crate) mod raw_block_cache;
pub mod receiver;
pub mod sample;
pub mod scheduler;
pub mod sender;
pub mod type_registry;
pub mod watchdog;
pub mod waveform_index;

pub use capture::{
    BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureMetadata,
    CaptureSampledChannel, CaptureSampledWindow, CaptureSource, CaptureTransition,
    CaptureWaveformSegment, DslHeader, DslSampledChannel, DslSampledWindow, DslTransition,
    DslWaveformSegment, packed_bit,
};
pub use errors::{ConnectionError, PortError, WorkError, WorkResult};
pub use events::{NumberSample, TextSample, Trigger};
pub use graph::{Connection, GraphBuilder, NodeId};
pub use manager::{DisconnectEvent, InputSub, NodeSpec, PipelineManager};
pub use node::{ConfigOutcome, ConfigValue, NodeConfig, ProcessNode};
pub use ports::{InputPort, OutputPort, Pipeline, PortDirection, PortSchema, register_type};
pub use receiver::{Receiver, ReceiverSelector};
pub use sample::Sample;
pub use sample::SampleBlock;
pub use scheduler::{Scheduler, StopHandle};
pub use sender::ChannelMessage;
pub use sender::{OverflowPolicy, Sender, SharedSenders};
pub use watchdog::Watchdog;
pub use waveform_index::{CaptureIndexProgress, IndexSampler, exact_window_sample_limit};
