//! UI-independent signal-processing runtime and capture infrastructure.
//!
//! This library provides a memory-efficient node runtime for processing captured and live
//! signals. Concrete logic-analyzer sources, processors, and sinks live in
//! `logic-analyzer-processing`.
//!
//! # Architecture
//!
//! - **Capture contracts**: Generic interfaces for sampled and indexed signals
//! - **Streaming Nodes**: Thread-per-node execution with crossbeam channels
//! - **Scheduler**: Manages node lifecycle and parallel execution
//! - **Derived data**: Generic viewer-lane storage and queries

pub mod capture;
mod capture_policy;
mod cooperative_manager;
mod derived_index;
pub mod derived_word_store;
pub mod edge_query;
pub mod errors;
pub mod events;
mod graph;
pub mod live_capture;
pub mod live_capture_store;
pub mod live_capture_waveform;
mod manager;
pub mod node;
pub mod pipeline;
pub mod ports;
pub mod protocol;
pub mod receiver;
pub mod sample;
pub mod sample_kind;
mod sampling_activity;
pub mod scheduler;
pub mod sender;
mod type_registry;
mod viewer_sink;
pub mod watchdog;

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "idle_wasm.rs"]
        mod idle;

        pub type AppManager = CooperativeManager;
    }
    _ => {
        #[path = "idle_native.rs"]
        mod idle;
        mod raw_block_cache;
        pub mod waveform_index;
        pub mod worker_pool;

        pub type AppManager = PipelineManager;
        pub use derived_word_store::{
            DecodedBlockCacheStats, cleanup_cache, clear_cache, clear_cache_entry,
            configure_decoded_block_cache, decoded_block_cache_stats, default_cache_directory,
            reset_decoded_block_cache_stats,
        };
        pub use waveform_index::{CaptureIndexProgress, IndexSampler, exact_window_sample_limit};
    }
}

pub use capture::{
    BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureIndex,
    CaptureMetadata, CaptureSampledChannel, CaptureSampledWindow, CaptureSource, CaptureTransition,
    CaptureWaveformSegment, DslHeader, DslSampledChannel, DslSampledWindow, DslTransition,
    DslWaveformSegment, packed_bit,
};
pub use capture_policy::{
    CaptureCapacityEstimate, CaptureCapacityRequest, CaptureFraction, CapturePolicy,
    CapturePolicyCapabilities, CapturePolicyContext, CapturePolicyError, CaptureRetentionPin,
    CaptureRetentionTracker, CaptureSessionPlan, CaptureStartMode, CompletionPolicy,
    CompletionPolicyKind, EffectiveCapturePolicy, RecordingStart, RetentionPolicy,
    RetentionPolicyKind, TriggerPlacement, TriggerPlacementCapability, TriggerTimeout,
    TriggerTimeoutAction, estimate_capture_capacity,
};
pub use cooperative_manager::CooperativeManager;
pub use derived_index::{AppendOnlyMipmap, ChunkedMipmap, LaneFold, MipmapRecord};
pub use derived_word_store::{
    AnnotationQuery, BlockCodecConfig, IndexedAnnotationStore, IndexedAnnotationWriter,
    LiveStoreConfig, PersistentStoreConfig, StoreStatus, WordPresenceBucket,
};
pub use edge_query::EdgeQuery;
pub use errors::{ConnectionError, DslError, Error, PortError, Result, WorkError, WorkResult};
pub use events::{
    Annotation, MAX_ANNOTATION_NS, NumberSample, TextSample, Trigger, Word,
    instantaneous_word_end_ns,
};
pub use graph::{Connection, GraphBuilder, NodeId};
pub(crate) use idle::idle_backoff;
pub use live_capture::{
    CAPTURE_CHUNK_FORMAT_VERSION, CaptureAcquisitionPhase, CaptureBufferLease, CaptureBufferPool,
    CaptureBufferPoolError, CaptureBufferPoolMetrics, CaptureBytes, CaptureChannelId, CaptureChunk,
    CaptureChunkError, CaptureChunkPayload, CaptureChunkWriter, CaptureCommandCapabilities,
    CaptureCompletion, CaptureDataDelivery, CaptureEvent, CaptureEventPublishError,
    CaptureEventPublisher, CaptureEventQueuePublisher, CaptureEventQueueReader, CaptureFailure,
    CaptureFailureKind, CaptureHealth, CaptureProgress, CaptureProviderCapabilities,
    CaptureQueueConfigError, CaptureQueueLimits, CaptureQueueReader, CaptureQueueReceiveError,
    CaptureQueueWriter, CaptureSessionId, CaptureSessionState, CaptureSettingCombination,
    CaptureStatus, CaptureWriteError, SimpleTriggerCondition, bounded_capture_event_queue,
    bounded_capture_queue,
};
pub use live_capture_store::*;
pub use manager::{DisconnectEvent, InputSub, NodeSpec, PipelineManager};
pub use node::{
    ConfigOutcome, ConfigValue, ConfigurationBoundary, ConfigurationScheduler,
    InputProtocolCandidate, NodeConfig, ProcessNode,
};
pub use pipeline::Pipeline;
pub use ports::{InputPort, OutputPort, PortDirection, PortSchema, register_type};
pub use protocol::ProtocolKind;
pub use receiver::{Receiver, ReceiverSelector};
pub use sample::{Sample, SampleBlock};
pub use sample_kind::SampleKind;
pub use sampling_activity::SamplingActivity;
pub use scheduler::{Scheduler, StopHandle};
pub use sender::{ChannelMessage, OverflowPolicy, Sender, SharedSenders};
pub use viewer_sink::{
    AnnotationFold, DEFAULT_VIEWER_MAX_ENTRIES, DerivedLane, DerivedLaneData, DerivedLanes,
    DigitalFold, IndexedAnnotationLane, LaneSummary, MarkerFold, ValueFold, ViewerLaneKind,
    ViewerRetention, ViewerSink, ViewerSinkMetrics, ViewerSinkMetricsSnapshot, ViewerValue,
    ViewerValueKind, ViewerValueLane,
};
pub use watchdog::Watchdog;
