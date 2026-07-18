//! Portable live-acquisition lifecycle for concrete logic-analyzer providers.

use thiserror::Error;

use signal_processing::{
    CaptureAcquisitionPhase, CaptureChunk, CaptureChunkWriter, CaptureEvent,
    CaptureEventPublishError, CaptureEventPublisher, CaptureFailure, CaptureFailureKind,
    CaptureProgress, CaptureSessionId, CaptureSessionState, CaptureStatus, CaptureWriteError,
};

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        mod fake_native;

        pub use fake_native::{
            DeterministicFakeConfig, DeterministicFakeController, DeterministicFakeProvider,
        };
    }
}

pub type AcquisitionResult<T> = Result<T, AcquisitionError>;
pub type LogicCaptureEvent = CaptureEvent;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AcquisitionError {
    #[error("invalid acquisition request: {0}")]
    InvalidRequest(String),
    #[error("acquisition has already started")]
    AlreadyStarted,
    #[error("acquisition has not been started")]
    NotStarted,
    #[error("capture writer failed: {0}")]
    Writer(#[from] CaptureWriteError),
    #[error("capture status publication failed: {0}")]
    Event(#[from] CaptureEventPublishError),
    #[error("acquisition transport failed: {0}")]
    Transport(String),
    #[error("acquisition protocol failed: {0}")]
    Protocol(String),
    #[error("acquisition was cancelled")]
    Cancelled,
    #[error("acquisition worker panicked")]
    WorkerPanicked,
    #[error("acquisition worker could not be started: {0}")]
    WorkerStart(String),
    #[error("acquisition failed: {0}")]
    Internal(String),
}

impl AcquisitionError {
    pub fn failure_kind(&self) -> CaptureFailureKind {
        match self {
            Self::InvalidRequest(_) => CaptureFailureKind::InvalidRequest,
            Self::Writer(_) => CaptureFailureKind::Writer,
            Self::Transport(_) => CaptureFailureKind::Transport,
            Self::Protocol(_) => CaptureFailureKind::Protocol,
            Self::Cancelled => CaptureFailureKind::Cancelled,
            Self::AlreadyStarted
            | Self::NotStarted
            | Self::Event(_)
            | Self::WorkerPanicked
            | Self::WorkerStart(_)
            | Self::Internal(_) => CaptureFailureKind::Internal,
        }
    }
}

/// Dependencies supplied to a prepared acquisition without exposing a store implementation.
pub struct AcquisitionContext {
    session_id: CaptureSessionId,
    writer: Box<dyn CaptureChunkWriter>,
    events: Box<dyn CaptureEventPublisher>,
}

impl AcquisitionContext {
    pub fn new(
        session_id: CaptureSessionId,
        writer: Box<dyn CaptureChunkWriter>,
        events: Box<dyn CaptureEventPublisher>,
    ) -> Self {
        Self {
            session_id,
            writer,
            events,
        }
    }

    pub const fn session_id(&self) -> CaptureSessionId {
        self.session_id
    }

    pub fn append(&mut self, chunk: CaptureChunk) -> AcquisitionResult<()> {
        if chunk.session_id() != self.session_id {
            return Err(AcquisitionError::InvalidRequest(format!(
                "chunk belongs to session {}, expected {}",
                chunk.session_id(),
                self.session_id
            )));
        }
        self.writer.append(chunk)?;
        Ok(())
    }

    pub fn finish_writer(&mut self) -> AcquisitionResult<()> {
        self.writer.finish()?;
        Ok(())
    }

    pub fn publish_status(
        &mut self,
        state: CaptureSessionState,
        phase: CaptureAcquisitionPhase,
    ) -> AcquisitionResult<()> {
        self.events.publish(CaptureEvent::Status(CaptureStatus {
            session_id: self.session_id,
            state,
            phase,
        }))?;
        Ok(())
    }

    pub fn publish_progress(&mut self, progress: CaptureProgress) -> AcquisitionResult<()> {
        self.events.publish(CaptureEvent::Progress {
            session_id: self.session_id,
            progress,
        })?;
        Ok(())
    }

    pub fn publish_failure(&mut self, error: &AcquisitionError) {
        let _ = self
            .events
            .publish(CaptureEvent::Failed(CaptureFailure::new(
                self.session_id,
                error.failure_kind(),
                error.to_string(),
            )));
        let _ = self.events.publish(CaptureEvent::Status(CaptureStatus {
            session_id: self.session_id,
            state: CaptureSessionState::Error,
            phase: CaptureAcquisitionPhase::Finalizing,
        }));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcquisitionOutcome {
    pub session_id: CaptureSessionId,
    pub captured_samples: u64,
    pub chunk_count: u64,
    pub stopped: bool,
}

/// Object-safe ownership boundary returned after a provider has prepared a session.
pub trait PreparedAcquisition: Send {
    fn session_id(&self) -> CaptureSessionId;
    fn start(&mut self) -> AcquisitionResult<()>;
    fn request_stop(&self) -> AcquisitionResult<()>;
    /// Non-blocking completion probe used by an acquisition supervisor so
    /// Stop remains available while Join runs off the UI thread.
    fn is_finished(&self) -> bool;
    fn join(self: Box<Self>) -> AcquisitionResult<AcquisitionOutcome>;
}
