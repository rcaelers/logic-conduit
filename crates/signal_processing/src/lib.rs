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

#[cfg(test)]
mod architecture_tests;

mod advanced_trigger;
mod app_manager;
pub mod capture;
mod capture_policy;
mod collected_payload;
mod cooperative_manager;
mod derived_data_collector;
mod derived_index;
pub mod derived_word_store;
mod edge_query;
mod errors;
mod events;
mod graph;
pub mod live_capture;
pub mod live_capture_store;
mod manager;
mod node;
mod pipeline;
mod ports;
mod protocol;
mod receiver;
mod sample;
mod sample_kind;
mod sampling_activity;
mod scheduler;
mod sender;
mod type_registry;
mod watchdog;

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "idle_wasm.rs"]
        mod idle;

    }
    _ => {
        mod archive_capture_store;
        mod crc32c;
        #[path = "idle_native.rs"]
        mod idle;
        pub mod waveform_index;
        mod worker_pool;

        pub use derived_word_store::{
            DecodedBlockCacheStats, cleanup_cache, clear_cache, clear_cache_entry,
            configure_decoded_block_cache, decoded_block_cache_stats, reset_decoded_block_cache_stats,
        };
        pub use waveform_index::{
            CaptureIndexProgress, IndexSampler, NativeGrowingCaptureIndex,
            NativeGrowingCaptureIndexWorker, exact_window_sample_limit,
        };
        pub use worker_pool::{WorkerPool, WorkerPoolStopped, shared_worker_pool};
    }
}

pub use advanced_trigger::{
    RegisteredTriggerPredicateSchema, TRIGGER_PROGRAM_FORMAT_VERSION, TriggerChoice, TriggerCount,
    TriggerCountCapabilities, TriggerCountMode, TriggerEditorSchema, TriggerIdentifier,
    TriggerLogicOperator, TriggerOperandKind, TriggerOperandSchema, TriggerOperandValue,
    TriggerPredicate, TriggerProgram, TriggerProgramEditError, TriggerProgramForm, TriggerStage,
    TriggerValidationCode, TriggerValidationDiagnostic, TriggerValidationErrors,
    ValidatedTriggerProgram,
};
pub use app_manager::AppManager;
pub use capture::{
    BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureIndex,
    CaptureIndexBuildProgress, CaptureIndexFactory, CaptureMetadata, CaptureSampledChannel,
    CaptureSampledWindow, CaptureSource, CaptureTransition, CaptureWaveformSegment,
    IndexedCapturePresentation, packed_bit,
};
pub use capture_policy::{
    CaptureFraction, CapturePolicy, CapturePolicyCapabilities, CapturePolicyContext,
    CapturePolicyError, CaptureRetentionPin, CaptureRetentionTracker, CaptureSessionPlan,
    CaptureStartMode, CompletionPolicy, CompletionPolicyKind, EffectiveCapturePolicy,
    RecordingStart, RetentionPolicy, RetentionPolicyKind, TriggerPlacement,
    TriggerPlacementCapability, TriggerTimeout, TriggerTimeoutAction,
};
pub use collected_payload::{
    CollectedLaneIngestor, CollectedLaneRequest, CollectedPayloadAdapter,
    CollectedPayloadDescriptor, CollectedPayloadRegistrationError, CollectedPayloadRegistry,
};
pub use cooperative_manager::CooperativeManager;
pub use derived_data_collector::{
    AnnotationFold, CollectedValue, CollectedValueKind, CollectedValueLane,
    CollectedWordLaneOptions, DEFAULT_DERIVED_DATA_MAX_ENTRIES, DerivedDataCollector,
    DerivedDataCollectorMetrics, DerivedDataCollectorMetricsSnapshot, DerivedDataRetention,
    DerivedLane, DerivedLaneData, DerivedLanes, DigitalFold, IndexedAnnotationLane, LaneSummary,
    MarkerFold, OpaqueCollectedLane, ValueFold, built_in_word_lane_ingestor,
    register_builtin_collected_payload_adapters,
};
pub use derived_index::{AppendOnlyMipmap, ChunkedMipmap, LaneFold, MipmapRecord};
pub use derived_word_store::{
    AnnotationQuery, BlockCodecConfig, IndexedAnnotationStore, IndexedAnnotationWriter,
    LiveStoreConfig, PersistentStoreConfig, StoreStatus, WordPresenceBucket,
};
pub use edge_query::EdgeQuery;
pub use errors::{ConnectionError, Error, PortError, Result, WorkError, WorkResult};
pub use events::{
    Annotation, MAX_ANNOTATION_NS, NumberSample, TextSample, Trigger, Word,
    instantaneous_word_end_ns,
};
pub use graph::{Connection, GraphBuilder, NodeId};
use idle::idle_backoff;
pub use live_capture::{
    AcquisitionContext, AcquisitionError, AcquisitionOutcome, AcquisitionResult,
    CAPTURE_CHUNK_FORMAT_VERSION, CaptureAcquisitionPhase, CaptureAnalysisChannel,
    CaptureAnalysisSource, CaptureBufferLease, CaptureBufferPool, CaptureBufferPoolError,
    CaptureBufferPoolMetrics, CaptureBytes, CaptureChannelId, CaptureChunk, CaptureChunkError,
    CaptureChunkPayload, CaptureChunkWriter, CaptureCommandCapabilities, CaptureCompletion,
    CaptureDataDelivery, CaptureEvent, CaptureEventPublishError, CaptureEventPublisher,
    CaptureEventQueuePublisher, CaptureEventQueueReader, CaptureFailure, CaptureFailureKind,
    CaptureHealth, CaptureProgress, CaptureProviderCapabilities, CaptureQueueConfigError,
    CaptureQueueLimits, CaptureQueueReader, CaptureQueueReceiveError, CaptureQueueWriter,
    CaptureSessionId, CaptureSessionState, CaptureSettingCombination, CaptureStatus,
    CaptureWriteError, PreparedAcquisition, SimpleTriggerCondition, bounded_capture_event_queue,
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
pub(crate) use sample_kind::negotiate as negotiate_sample_kind;
pub use sampling_activity::SamplingActivity;
pub use scheduler::{Scheduler, StopHandle};
pub use sender::{ChannelMessage, OverflowPolicy, Sender, SharedSenders};
pub(crate) use watchdog::OperationGuard;
pub use watchdog::{Watchdog, WatchdogHandle};
