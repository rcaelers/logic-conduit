//! Platform-neutral contract for an authoritative live-capture store.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CaptureChannelId, CaptureChunk, CaptureSessionId};

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        #[path = "native.rs"]
        mod native;
        #[path = "repository_native.rs"]
        mod repository_native;

        pub use native::{
            NativeCaptureCursor, NativeCaptureRandomReader, NativeCaptureStore,
            NativeCaptureStoreConfig, NativeCaptureStoreWriter, NativeFinalizedCapture,
        };
        pub use repository_native::{
            CaptureSessionCleanupPlan, NativeCaptureSessionPin, NativeCaptureSessionRepository,
            NativeCaptureSessionRepositoryConfig, NativeCaptureSessionSummary,
        };
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureStoreDescriptor {
    session_id: CaptureSessionId,
    channels: Arc<[CaptureChannelId]>,
}

impl CaptureStoreDescriptor {
    pub fn new(
        session_id: CaptureSessionId,
        channels: impl Into<Arc<[CaptureChannelId]>>,
    ) -> CaptureStoreResult<Self> {
        let channels = channels.into();
        if channels.is_empty() {
            return Err(CaptureStoreError::InvalidConfig(
                "capture store requires at least one channel".into(),
            ));
        }
        Ok(Self {
            session_id,
            channels,
        })
    }

    pub const fn session_id(&self) -> CaptureSessionId {
        self.session_id
    }

    pub fn channels(&self) -> &[CaptureChannelId] {
        &self.channels
    }

    pub fn channel_table(&self) -> Arc<[CaptureChannelId]> {
        Arc::clone(&self.channels)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureStoreSnapshot {
    pub committed_chunks: u64,
    pub committed_samples: u64,
    pub committed_data_bytes: u64,
    pub writer_open: bool,
    pub writer_failed: bool,
    pub finalized: bool,
    /// Commit records remain on disk; the live store retains none in an in-memory vector.
    pub resident_commit_records: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureStoreManifest {
    pub descriptor: CaptureStoreDescriptor,
    pub committed_chunks: u64,
    pub committed_samples: u64,
    pub committed_data_bytes: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSessionOutcome {
    InProgress,
    Complete,
    Stopped,
    CancelledBeforeTrigger,
    Incomplete,
    Aborted,
    Corrupt,
}

impl CaptureSessionOutcome {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::InProgress)
    }

    pub const fn is_incomplete(self) -> bool {
        matches!(
            self,
            Self::CancelledBeforeTrigger | Self::Incomplete | Self::Aborted
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureSessionMetadata {
    pub descriptor: CaptureStoreDescriptor,
    pub timeline: Option<CaptureTimelineMetadata>,
    pub outcome: CaptureSessionOutcome,
    pub created_unix_ns: u64,
    pub accessed_unix_ns: u64,
    pub recording_origin: Option<u64>,
    pub retained_start_sample: u64,
    pub kept: bool,
}

/// Durable, format-neutral presentation metadata for reopening and exporting a capture.
///
/// The sample rate is retained by its IEEE-754 representation so equality is exact while
/// callers continue to use the same `f64` timebase contract as capture sources and viewers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureTimelineMetadata {
    sample_rate_hz_bits: u64,
    channel_names: Vec<String>,
    trigger_sample: Option<u64>,
}

impl CaptureTimelineMetadata {
    pub fn new(sample_rate_hz: f64, channel_names: Vec<String>) -> CaptureStoreResult<Self> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err(CaptureStoreError::InvalidConfig(
                "capture sample rate must be finite and positive".into(),
            ));
        }
        if channel_names.is_empty() {
            return Err(CaptureStoreError::InvalidConfig(
                "capture timeline requires at least one channel name".into(),
            ));
        }
        Ok(Self {
            sample_rate_hz_bits: sample_rate_hz.to_bits(),
            channel_names,
            trigger_sample: None,
        })
    }

    pub fn sample_rate_hz(&self) -> f64 {
        f64::from_bits(self.sample_rate_hz_bits)
    }

    pub fn channel_names(&self) -> &[String] {
        &self.channel_names
    }

    pub const fn trigger_sample(&self) -> Option<u64> {
        self.trigger_sample
    }

    pub fn set_trigger_sample(&mut self, trigger_sample: Option<u64>) {
        self.trigger_sample = trigger_sample;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureRecoveryReport {
    pub recovered: bool,
    pub truncated_data_bytes: u64,
    pub truncated_commit_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureReclamationReport {
    pub reclaimed_chunks: u64,
    pub reclaimed_samples: u64,
    pub reclaimed_data_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureCursorItem {
    Chunk(CaptureChunk),
    Pending,
    End,
}

pub trait CaptureStoreCursor: Send {
    fn next(&mut self) -> CaptureStoreResult<CaptureCursorItem>;
    fn wait_next(&mut self, timeout: Duration) -> CaptureStoreResult<CaptureCursorItem>;
    fn next_sequence(&self) -> u64;

    /// Sample offset that maps this cursor's zero-based processing timeline
    /// back onto the authoritative capture timeline for timestamped output.
    fn timeline_offset_samples(&self) -> u64 {
        0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordingGateState {
    Pending,
    Origin(u64),
    NoRecording,
}

#[derive(Debug)]
struct RecordingGateInner {
    state: Mutex<RecordingGateState>,
    changed: Condvar,
}

/// Shared recording-origin decision for live and finalized capture cursors.
///
/// The authoritative store always retains the raw capture. Consumers behind this gate remain
/// pending until a trigger resolves the recording origin, then receive only post-origin chunks on
/// a zero-based sample timeline.
#[derive(Clone, Debug)]
pub struct CaptureRecordingGate {
    inner: Arc<RecordingGateInner>,
}

impl CaptureRecordingGate {
    pub fn immediate() -> Self {
        Self::with_state(RecordingGateState::Origin(0))
    }

    pub fn pending() -> Self {
        Self::with_state(RecordingGateState::Pending)
    }

    pub fn finalized(origin_sample: Option<u64>) -> Self {
        Self::with_state(match origin_sample {
            Some(origin_sample) => RecordingGateState::Origin(origin_sample),
            None => RecordingGateState::NoRecording,
        })
    }

    fn with_state(state: RecordingGateState) -> Self {
        Self {
            inner: Arc::new(RecordingGateInner {
                state: Mutex::new(state),
                changed: Condvar::new(),
            }),
        }
    }

    pub fn resolve_trigger(&self, sample: u64) -> bool {
        self.resolve(RecordingGateState::Origin(sample))
    }

    pub fn finish_without_trigger(&self) -> bool {
        self.resolve(RecordingGateState::NoRecording)
    }

    fn resolve(&self, resolution: RecordingGateState) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *state != RecordingGateState::Pending {
            return false;
        }
        *state = resolution;
        self.inner.changed.notify_all();
        true
    }

    pub fn recording_origin(&self) -> Option<u64> {
        match *self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            RecordingGateState::Origin(sample) => Some(sample),
            RecordingGateState::Pending | RecordingGateState::NoRecording => None,
        }
    }

    pub fn is_resolved(&self) -> bool {
        *self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            != RecordingGateState::Pending
    }

    pub fn cursor(&self, cursor: Box<dyn CaptureStoreCursor>) -> RecordingCaptureCursor {
        RecordingCaptureCursor {
            cursor,
            gate: self.clone(),
            output_sequence: 0,
        }
    }
}

pub struct RecordingCaptureCursor {
    cursor: Box<dyn CaptureStoreCursor>,
    gate: CaptureRecordingGate,
    output_sequence: u64,
}

impl RecordingCaptureCursor {
    fn state(&self) -> RecordingGateState {
        *self
            .gate
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn translate(
        &mut self,
        item: CaptureCursorItem,
    ) -> CaptureStoreResult<Option<CaptureCursorItem>> {
        let origin_sample = match self.state() {
            RecordingGateState::Pending => return Ok(Some(CaptureCursorItem::Pending)),
            RecordingGateState::NoRecording => return Ok(Some(CaptureCursorItem::End)),
            RecordingGateState::Origin(sample) => sample,
        };
        let CaptureCursorItem::Chunk(chunk) = item else {
            return Ok(Some(item));
        };
        match chunk
            .recording_slice(origin_sample, self.output_sequence)
            .map_err(|error| CaptureStoreError::InvalidChunk(error.to_string()))?
        {
            Some(chunk) => {
                self.output_sequence += 1;
                Ok(Some(CaptureCursorItem::Chunk(chunk)))
            }
            None => Ok(None),
        }
    }
}

impl CaptureStoreCursor for RecordingCaptureCursor {
    fn next(&mut self) -> CaptureStoreResult<CaptureCursorItem> {
        match self.state() {
            RecordingGateState::Pending => Ok(CaptureCursorItem::Pending),
            RecordingGateState::NoRecording => Ok(CaptureCursorItem::End),
            RecordingGateState::Origin(_) => loop {
                let item = self.cursor.next()?;
                if let Some(item) = self.translate(item)? {
                    break Ok(item);
                }
            },
        }
    }

    fn wait_next(&mut self, timeout: Duration) -> CaptureStoreResult<CaptureCursorItem> {
        let state = self
            .gate
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (state_guard, _) = self
            .gate
            .inner
            .changed
            .wait_timeout_while(state, timeout, |state| {
                *state == RecordingGateState::Pending
            })
            .unwrap_or_else(|error| error.into_inner());
        let state = *state_guard;
        drop(state_guard);
        match state {
            RecordingGateState::Pending => Ok(CaptureCursorItem::Pending),
            RecordingGateState::NoRecording => Ok(CaptureCursorItem::End),
            RecordingGateState::Origin(_) => {
                let item = self.cursor.wait_next(timeout)?;
                match self.translate(item)? {
                    Some(item) => Ok(item),
                    None => self.next(),
                }
            }
        }
    }

    fn next_sequence(&self) -> u64 {
        self.output_sequence
    }

    fn timeline_offset_samples(&self) -> u64 {
        self.gate.recording_origin().unwrap_or(0)
    }
}

pub type CaptureStoreResult<T> = Result<T, CaptureStoreError>;

#[derive(Debug, Error)]
pub enum CaptureStoreError {
    #[error("invalid capture-store configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid capture chunk: {0}")]
    InvalidChunk(String),
    #[error("capture-store writer is still open")]
    WriterStillOpen,
    #[error("capture store is already finalized")]
    AlreadyFinalized,
    #[error("capture store is not finalized")]
    NotFinalized,
    #[error("capture store failed: {0}")]
    WriterFailed(String),
    #[error("corrupt capture store: {0}")]
    Corrupt(String),
    #[error("capture session {0} is pinned and cannot be removed")]
    SessionPinned(CaptureSessionId),
    #[error("capture session {0} was not found")]
    SessionNotFound(CaptureSessionId),
    #[error("capture-store I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{CaptureCursorItem, CaptureRecordingGate, CaptureStoreCursor, CaptureStoreResult};
    use crate::{CaptureChannelId, CaptureChunk, CaptureSessionId};

    struct MemoryCursor {
        items: VecDeque<CaptureCursorItem>,
        next_sequence: u64,
    }

    impl CaptureStoreCursor for MemoryCursor {
        fn next(&mut self) -> CaptureStoreResult<CaptureCursorItem> {
            let item = self.items.pop_front().unwrap_or(CaptureCursorItem::End);
            if matches!(item, CaptureCursorItem::Chunk(_)) {
                self.next_sequence += 1;
            }
            Ok(item)
        }

        fn wait_next(&mut self, _timeout: Duration) -> CaptureStoreResult<CaptureCursorItem> {
            self.next()
        }

        fn next_sequence(&self) -> u64 {
            self.next_sequence
        }
    }

    fn chunk(sequence: u64, start_sample: u64) -> CaptureChunk {
        CaptureChunk::packed_lsb_first(
            CaptureSessionId::new(1),
            sequence,
            start_sample,
            4,
            Arc::from([CaptureChannelId::new("opaque:7")]),
            [0b0000_1010],
            0,
        )
        .unwrap()
    }

    fn cursor() -> Box<dyn CaptureStoreCursor> {
        Box::new(MemoryCursor {
            items: VecDeque::from([
                CaptureCursorItem::Chunk(chunk(0, 0)),
                CaptureCursorItem::Chunk(chunk(1, 4)),
                CaptureCursorItem::Chunk(chunk(2, 8)),
                CaptureCursorItem::End,
            ]),
            next_sequence: 0,
        })
    }

    #[test]
    fn pending_gate_releases_a_zero_based_post_trigger_stream() {
        let gate = CaptureRecordingGate::pending();
        let mut cursor = gate.cursor(cursor());
        assert_eq!(cursor.next().unwrap(), CaptureCursorItem::Pending);
        assert!(gate.resolve_trigger(6));
        assert!(!gate.resolve_trigger(7));

        let CaptureCursorItem::Chunk(first) = cursor.next().unwrap() else {
            panic!("expected trigger-crossing chunk");
        };
        assert_eq!(first.sequence(), 0);
        assert_eq!(first.start_sample(), 0);
        assert_eq!(first.sample_count(), 2);
        assert_eq!(first.packed_level(0, 0), Some(false));
        assert_eq!(first.packed_level(1, 0), Some(true));

        let CaptureCursorItem::Chunk(second) = cursor.next().unwrap() else {
            panic!("expected post-trigger chunk");
        };
        assert_eq!(second.sequence(), 1);
        assert_eq!(second.start_sample(), 2);
        assert_eq!(cursor.next().unwrap(), CaptureCursorItem::End);
    }

    #[test]
    fn unresolved_capture_finishes_as_an_empty_recording() {
        let gate = CaptureRecordingGate::pending();
        let mut cursor = gate.cursor(cursor());
        assert!(gate.finish_without_trigger());
        assert_eq!(cursor.next().unwrap(), CaptureCursorItem::End);
        assert_eq!(cursor.next_sequence(), 0);
    }
}
