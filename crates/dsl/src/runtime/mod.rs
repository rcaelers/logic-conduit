//! Runtime support for streaming node graphs

pub mod capture;
pub mod errors;
pub mod graph;
pub mod node;
pub mod pipeline;
pub mod ports;
pub mod receiver;
pub mod sample;
pub mod scheduler;
pub mod sender;
pub mod type_registry;
pub mod watchdog;
pub mod waveform_index;

pub use capture::{
    BlockCaptureSource, CaptureActivity, CaptureBucket, CaptureDataSource, CaptureFingerprint,
    CaptureMetadata, CaptureSampledChannel, CaptureSampledWindow, CaptureSource, CaptureTransition,
    DslActivity, DslBucket, DslHeader, DslSampledChannel, DslSampledWindow, DslTransition,
    packed_bit,
};
pub use errors::{ConnectionError, PortError, WorkError, WorkResult};
pub use graph::{Connection, GraphBuilder, NodeId};
pub use node::ProcessNode;
pub use ports::{InputPort, OutputPort, Pipeline, PortDirection, PortSchema, register_type};
pub use receiver::{Receiver, ReceiverSelector};
pub use sample::Sample;
pub use sample::SampleBlock;
pub use scheduler::Scheduler;
pub use sender::ChannelMessage;
pub use sender::Sender;
pub use watchdog::Watchdog;
pub use waveform_index::{CaptureIndexProgress, IndexSampler};
