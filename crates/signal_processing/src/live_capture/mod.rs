//! Provider-neutral live-capture contracts, events, queues, and buffers.

mod acquisition;
mod analysis;
mod implementation;

pub use acquisition::{
    AcquisitionContext, AcquisitionError, AcquisitionOutcome, AcquisitionResult,
    PreparedAcquisition,
};
pub use analysis::{CaptureAnalysisChannel, CaptureAnalysisSource};
pub use implementation::{
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
