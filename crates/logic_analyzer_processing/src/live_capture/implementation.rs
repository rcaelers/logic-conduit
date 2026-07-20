//! Portable live-acquisition lifecycle for concrete logic-analyzer providers.

use thiserror::Error;

use signal_processing::{
    CaptureAcquisitionPhase, CaptureChunk, CaptureChunkWriter, CaptureCompletion, CaptureEvent,
    CaptureEventPublishError, CaptureEventPublisher, CaptureFailure, CaptureFailureKind,
    CaptureHealth, CaptureProgress, CaptureSessionId, CaptureSessionPlan, CaptureSessionState,
    CaptureStatus, CaptureWriteError,
};

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
    #[error("unsupported acquisition operation: {0}")]
    UnsupportedOperation(String),
    #[error("capture writer failed: {0}")]
    Writer(#[from] CaptureWriteError),
    #[error("capture status publication failed: {0}")]
    Event(#[from] CaptureEventPublishError),
    #[error("acquisition transport failed: {0}")]
    Transport(String),
    #[error("acquisition protocol failed: {0}")]
    Protocol(String),
    #[error("capture integrity was lost: {0}")]
    Integrity(String),
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
            Self::UnsupportedOperation(_) => CaptureFailureKind::InvalidRequest,
            Self::Writer(_) => CaptureFailureKind::Writer,
            Self::Transport(_) => CaptureFailureKind::Transport,
            Self::Protocol(_) => CaptureFailureKind::Protocol,
            Self::Integrity(_) => CaptureFailureKind::Integrity,
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

    pub fn publish_health(&mut self, health: CaptureHealth) -> AcquisitionResult<()> {
        self.events.publish(CaptureEvent::Health {
            session_id: self.session_id,
            health,
        })?;
        Ok(())
    }

    pub fn publish_plan(&mut self, plan: CaptureSessionPlan) -> AcquisitionResult<()> {
        self.events.publish(CaptureEvent::Plan {
            session_id: self.session_id,
            plan,
        })?;
        Ok(())
    }

    pub fn publish_triggered(&mut self, sample: u64) -> AcquisitionResult<()> {
        self.events.publish(CaptureEvent::Triggered {
            session_id: self.session_id,
            sample,
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
    pub completion: CaptureCompletion,
}

/// Object-safe ownership boundary returned after a provider has prepared a session.
pub trait PreparedAcquisition: Send {
    fn session_id(&self) -> CaptureSessionId;
    fn start(&mut self) -> AcquisitionResult<()>;
    fn request_stop(&self) -> AcquisitionResult<()>;
    fn request_abort(&self) -> AcquisitionResult<()> {
        Err(AcquisitionError::UnsupportedOperation("abort".into()))
    }
    fn request_force_trigger(&self) -> AcquisitionResult<()> {
        Err(AcquisitionError::UnsupportedOperation(
            "force trigger".into(),
        ))
    }
    /// Non-blocking completion probe used by an acquisition supervisor so
    /// Stop remains available while Join runs off the UI thread.
    fn is_finished(&self) -> bool;
    fn join(self: Box<Self>) -> AcquisitionResult<AcquisitionOutcome>;
}
