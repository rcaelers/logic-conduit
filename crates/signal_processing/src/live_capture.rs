//! Generic, UI-independent contracts for bounded live-capture ingestion.
//!
//! Concrete devices and acquisition lifecycles live in `logic-analyzer-processing`. This module
//! owns only the canonical data, status, and writer boundaries shared by capture providers,
//! stores, graph cursors, and viewers.

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::advanced_trigger::{
    TriggerEditorSchema, TriggerProgram, TriggerValidationErrors, ValidatedTriggerProgram,
};
use crate::{CapturePolicyCapabilities, CaptureSessionPlan};

pub const CAPTURE_CHUNK_FORMAT_VERSION: u16 = 1;

/// Portable one-channel condition used by simple capture triggers.
///
/// Providers may lower this contract into a native device representation, or evaluate it in a
/// host-side acquisition implementation. Multiple enabled conditions are combined with AND.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SimpleTriggerCondition {
    #[default]
    Ignore,
    Low,
    High,
    Rising,
    Falling,
    Either,
}

impl SimpleTriggerCondition {
    pub const fn is_edge(self) -> bool {
        matches!(self, Self::Rising | Self::Falling | Self::Either)
    }

    pub const fn matches(self, previous: Option<bool>, current: bool) -> bool {
        match self {
            Self::Ignore => true,
            Self::Low => !current,
            Self::High => current,
            Self::Rising => matches!(previous, Some(false)) && current,
            Self::Falling => matches!(previous, Some(true)) && !current,
            Self::Either => match previous {
                Some(previous) => previous != current,
                None => false,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureSessionId(u128);

impl CaptureSessionId {
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u128 {
        self.0
    }
}

impl fmt::Display for CaptureSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:032x}", self.0)
    }
}

/// Provider-owned physical-channel identity. Generic code treats it as opaque.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureChannelId(Arc<str>);

impl CaptureChannelId {
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CaptureChannelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for CaptureChannelId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CaptureChannelId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::new)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureDataDelivery {
    /// Canonical chunks become available while acquisition is still sampling.
    DuringAcquisition,
    /// Sampling completes in provider-owned storage before canonical chunks are uploaded.
    BufferedUpload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureSettingCombination {
    channels: Arc<[CaptureChannelId]>,
    sample_rates_hz: Arc<[u64]>,
}

impl CaptureSettingCombination {
    pub fn new(
        channels: impl Into<Arc<[CaptureChannelId]>>,
        sample_rates_hz: impl Into<Arc<[u64]>>,
    ) -> Result<Self, String> {
        let channels = channels.into();
        let sample_rates_hz = sample_rates_hz.into();
        if channels.is_empty() {
            return Err("a capture setting combination requires at least one channel".into());
        }
        if sample_rates_hz.is_empty() || sample_rates_hz.contains(&0) {
            return Err("capture setting sample rates must be non-zero".into());
        }
        let unique_channels: HashSet<_> = channels.iter().collect();
        if unique_channels.len() != channels.len() {
            return Err("capture setting channels must be unique".into());
        }
        let unique_rates: HashSet<_> = sample_rates_hz.iter().collect();
        if unique_rates.len() != sample_rates_hz.len() {
            return Err("capture setting sample rates must be unique".into());
        }
        Ok(Self {
            channels,
            sample_rates_hz,
        })
    }

    pub fn channels(&self) -> &[CaptureChannelId] {
        &self.channels
    }

    pub fn sample_rates_hz(&self) -> &[u64] {
        &self.sample_rates_hz
    }

    pub fn supports(&self, channels: &[CaptureChannelId], sample_rate_hz: f64) -> bool {
        self.channels.as_ref() == channels
            && self
                .sample_rates_hz
                .iter()
                .any(|rate| *rate as f64 == sample_rate_hz)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureProviderCapabilities {
    data_delivery: CaptureDataDelivery,
    setting_matrix: Arc<[CaptureSettingCombination]>,
    commands: CaptureCommandCapabilities,
    policy: CapturePolicyCapabilities,
    trigger_schema: Option<Arc<TriggerEditorSchema>>,
}

impl CaptureProviderCapabilities {
    pub fn new(
        data_delivery: CaptureDataDelivery,
        setting_matrix: impl Into<Arc<[CaptureSettingCombination]>>,
        force_trigger: bool,
    ) -> Result<Self, String> {
        let setting_matrix = setting_matrix.into();
        if setting_matrix.is_empty() {
            return Err("capture capabilities require a non-empty setting matrix".into());
        }
        Ok(Self {
            data_delivery,
            setting_matrix,
            commands: CaptureCommandCapabilities {
                orderly_stop: true,
                abort: false,
                force_trigger,
                capture_now: true,
            },
            policy: CapturePolicyCapabilities::finite_default(),
            trigger_schema: None,
        })
    }

    pub fn single(
        data_delivery: CaptureDataDelivery,
        channels: impl Into<Arc<[CaptureChannelId]>>,
        sample_rate_hz: u64,
    ) -> Self {
        let setting = CaptureSettingCombination::new(channels, Arc::from([sample_rate_hz]))
            .expect("single capture capability is valid");
        Self::new(data_delivery, Arc::from([setting]), false)
            .expect("single capture capability is valid")
    }

    pub const fn data_delivery(&self) -> CaptureDataDelivery {
        self.data_delivery
    }

    pub fn setting_matrix(&self) -> &[CaptureSettingCombination] {
        &self.setting_matrix
    }

    pub const fn supports_force_trigger(&self) -> bool {
        self.commands.force_trigger
    }

    pub const fn commands(&self) -> CaptureCommandCapabilities {
        self.commands
    }

    pub fn policy(&self) -> &CapturePolicyCapabilities {
        &self.policy
    }

    pub fn with_commands(mut self, commands: CaptureCommandCapabilities) -> Self {
        self.commands = commands;
        self
    }

    pub fn with_policy(mut self, policy: CapturePolicyCapabilities) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_trigger_schema(mut self, schema: TriggerEditorSchema) -> Self {
        self.trigger_schema = Some(Arc::new(schema));
        self
    }

    pub fn trigger_schema(&self) -> Option<&TriggerEditorSchema> {
        self.trigger_schema.as_deref()
    }

    pub fn negotiate_trigger_program(
        &self,
        program: Option<&TriggerProgram>,
        channels: &[CaptureChannelId],
    ) -> Result<Option<ValidatedTriggerProgram>, TriggerValidationErrors> {
        let Some(program) = program else {
            return Ok(None);
        };
        let schema = self
            .trigger_schema()
            .ok_or_else(TriggerValidationErrors::schema_unavailable)?;
        schema.validate_program(program, channels).map(Some)
    }

    pub fn supports(&self, channels: &[CaptureChannelId], sample_rate_hz: f64) -> bool {
        self.setting_matrix
            .iter()
            .any(|setting| setting.supports(channels, sample_rate_hz))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureCommandCapabilities {
    pub orderly_stop: bool,
    pub abort: bool,
    pub force_trigger: bool,
    pub capture_now: bool,
}

impl CaptureCommandCapabilities {
    pub const fn new(
        orderly_stop: bool,
        abort: bool,
        force_trigger: bool,
        capture_now: bool,
    ) -> Self {
        Self {
            orderly_stop,
            abort,
            force_trigger,
            capture_now,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureSessionState {
    Preparing,
    Prepared,
    Armed,
    Triggered,
    Recording,
    Stopping,
    Complete,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureCompletion {
    Finished,
    Stopped,
    CancelledBeforeTrigger,
    Aborted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureAcquisitionPhase {
    Preparing,
    Ready,
    WaitingForTrigger,
    CapturingOnDevice,
    ReceivingLiveData,
    UploadingBufferedData,
    DrainingPipeline,
    Finalizing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureProgress {
    pub captured_samples: Option<u64>,
    pub transferred_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureHealth {
    pub input_bytes_per_second: Option<u64>,
    pub write_bytes_per_second: Option<u64>,
    pub buffer_used_bytes: Option<u64>,
    pub buffer_capacity_bytes: Option<u64>,
    pub available_storage_bytes: Option<u64>,
    pub retained_samples: Option<u64>,
    pub summary_lag_samples: Option<u64>,
    pub graph_lag_samples: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureStatus {
    pub session_id: CaptureSessionId,
    pub state: CaptureSessionState,
    pub phase: CaptureAcquisitionPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureFailureKind {
    InvalidRequest,
    Transport,
    Protocol,
    Integrity,
    Writer,
    Cancelled,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureFailure {
    pub session_id: CaptureSessionId,
    pub kind: CaptureFailureKind,
    pub message: String,
}

impl CaptureFailure {
    pub fn new(
        session_id: CaptureSessionId,
        kind: CaptureFailureKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            session_id,
            kind,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureEvent {
    Status(CaptureStatus),
    Progress {
        session_id: CaptureSessionId,
        progress: CaptureProgress,
    },
    Health {
        session_id: CaptureSessionId,
        health: CaptureHealth,
    },
    Plan {
        session_id: CaptureSessionId,
        plan: CaptureSessionPlan,
    },
    /// The raw capture sample at which all enabled simple trigger conditions matched.
    Triggered {
        session_id: CaptureSessionId,
        sample: u64,
    },
    Failed(CaptureFailure),
}

#[derive(Clone)]
pub struct CaptureBytes(Arc<CaptureBytesInner>);

enum CaptureBytesInner {
    Owned(Box<[u8]>),
    Shared(Arc<[u8]>),
    Pooled(PooledCaptureBytes),
}

struct PooledCaptureBytes {
    bytes: Vec<u8>,
    pool: Weak<CaptureBufferPoolInner>,
}

impl Drop for PooledCaptureBytes {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            pool.return_buffer(std::mem::take(&mut self.bytes));
        }
    }
}

impl CaptureBytes {
    pub fn as_slice(&self) -> &[u8] {
        match self.0.as_ref() {
            CaptureBytesInner::Owned(bytes) => bytes,
            CaptureBytesInner::Shared(bytes) => bytes,
            CaptureBytesInner::Pooled(bytes) => &bytes.bytes,
        }
    }

    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
}

impl AsRef<[u8]> for CaptureBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl fmt::Debug for CaptureBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureBytes")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl PartialEq for CaptureBytes {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for CaptureBytes {}

impl From<Vec<u8>> for CaptureBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(Arc::new(CaptureBytesInner::Owned(bytes.into_boxed_slice())))
    }
}

impl<const N: usize> From<[u8; N]> for CaptureBytes {
    fn from(bytes: [u8; N]) -> Self {
        Self::from(Vec::from(bytes))
    }
}

impl From<Arc<[u8]>> for CaptureBytes {
    fn from(bytes: Arc<[u8]>) -> Self {
        Self(Arc::new(CaptureBytesInner::Shared(bytes)))
    }
}

#[derive(Debug)]
struct CaptureBufferPoolState {
    available: Vec<Vec<u8>>,
    allocated: usize,
    in_use: usize,
    max_in_use: usize,
}

#[derive(Debug)]
struct CaptureBufferPoolInner {
    max_buffers: usize,
    initial_capacity: usize,
    state: Mutex<CaptureBufferPoolState>,
    available: Condvar,
}

impl CaptureBufferPoolInner {
    fn return_buffer(&self, mut buffer: Vec<u8>) {
        buffer.clear();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        debug_assert!(state.in_use > 0);
        state.in_use -= 1;
        state.available.push(buffer);
        self.available.notify_one();
    }
}

#[derive(Clone, Debug)]
pub struct CaptureBufferPool {
    inner: Arc<CaptureBufferPoolInner>,
}

impl CaptureBufferPool {
    pub fn new(
        max_buffers: usize,
        initial_capacity: usize,
    ) -> Result<Self, CaptureBufferPoolError> {
        if max_buffers == 0 {
            return Err(CaptureBufferPoolError::ZeroCapacity);
        }
        Ok(Self {
            inner: Arc::new(CaptureBufferPoolInner {
                max_buffers,
                initial_capacity,
                state: Mutex::new(CaptureBufferPoolState {
                    available: Vec::with_capacity(max_buffers),
                    allocated: 0,
                    in_use: 0,
                    max_in_use: 0,
                }),
                available: Condvar::new(),
            }),
        })
    }

    pub fn acquire(&self) -> CaptureBufferLease {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            let buffer = if let Some(buffer) = state.available.pop() {
                buffer
            } else if state.allocated < self.inner.max_buffers {
                state.allocated += 1;
                Vec::with_capacity(self.inner.initial_capacity)
            } else {
                state = self
                    .inner
                    .available
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
                continue;
            };
            state.in_use += 1;
            state.max_in_use = state.max_in_use.max(state.in_use);
            return CaptureBufferLease {
                buffer: Some(buffer),
                pool: Arc::clone(&self.inner),
            };
        }
    }

    pub fn metrics(&self) -> CaptureBufferPoolMetrics {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        CaptureBufferPoolMetrics {
            max_buffers: self.inner.max_buffers,
            allocated: state.allocated,
            available: state.available.len(),
            in_use: state.in_use,
            max_in_use: state.max_in_use,
        }
    }
}

pub struct CaptureBufferLease {
    buffer: Option<Vec<u8>>,
    pool: Arc<CaptureBufferPoolInner>,
}

impl CaptureBufferLease {
    pub fn resize(&mut self, len: usize, value: u8) {
        self.buffer
            .as_mut()
            .expect("live buffer lease owns its buffer")
            .resize(len, value);
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buffer
            .as_mut()
            .expect("live buffer lease owns its buffer")
            .as_mut_slice()
    }

    pub fn freeze(mut self) -> CaptureBytes {
        let bytes = self
            .buffer
            .take()
            .expect("live buffer lease owns its buffer");
        CaptureBytes(Arc::new(CaptureBytesInner::Pooled(PooledCaptureBytes {
            bytes,
            pool: Arc::downgrade(&self.pool),
        })))
    }
}

impl Drop for CaptureBufferLease {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            self.pool.return_buffer(buffer);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureBufferPoolMetrics {
    pub max_buffers: usize,
    pub allocated: usize,
    pub available: usize,
    pub in_use: usize,
    pub max_in_use: usize,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CaptureBufferPoolError {
    #[error("capture buffer pool capacity must be non-zero")]
    ZeroCapacity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureChunkPayload {
    /// Channel bits follow the chunk's channel table, least-significant bit first in each byte.
    PackedLsbFirst { bytes: CaptureBytes, bit_offset: u8 },
}

/// Immutable canonical raw data shared by acquisition, storage, and caught-up consumers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureChunk {
    format_version: u16,
    session_id: CaptureSessionId,
    sequence: u64,
    start_sample: u64,
    sample_count: u64,
    channels: Arc<[CaptureChannelId]>,
    payload: CaptureChunkPayload,
}

impl CaptureChunk {
    #[allow(clippy::too_many_arguments)]
    pub fn packed_lsb_first(
        session_id: CaptureSessionId,
        sequence: u64,
        start_sample: u64,
        sample_count: u64,
        channels: impl Into<Arc<[CaptureChannelId]>>,
        bytes: impl Into<CaptureBytes>,
        bit_offset: u8,
    ) -> Result<Self, CaptureChunkError> {
        let channels = channels.into();
        let bytes = bytes.into();
        if channels.is_empty() {
            return Err(CaptureChunkError::NoChannels);
        }
        if sample_count == 0 {
            return Err(CaptureChunkError::NoSamples);
        }
        if bit_offset >= 8 {
            return Err(CaptureChunkError::InvalidBitOffset(bit_offset));
        }
        start_sample
            .checked_add(sample_count)
            .ok_or(CaptureChunkError::SampleRangeOverflow)?;
        let required_bits = u128::from(sample_count)
            .checked_mul(channels.len() as u128)
            .ok_or(CaptureChunkError::PayloadSizeOverflow)?;
        let available_bits = (bytes.len() as u128)
            .checked_mul(8)
            .and_then(|bits| bits.checked_sub(u128::from(bit_offset)))
            .ok_or(CaptureChunkError::PayloadSizeOverflow)?;
        if required_bits > available_bits {
            return Err(CaptureChunkError::PayloadTooShort {
                required_bits,
                available_bits,
            });
        }
        Ok(Self {
            format_version: CAPTURE_CHUNK_FORMAT_VERSION,
            session_id,
            sequence,
            start_sample,
            sample_count,
            channels,
            payload: CaptureChunkPayload::PackedLsbFirst { bytes, bit_offset },
        })
    }

    pub const fn format_version(&self) -> u16 {
        self.format_version
    }

    pub const fn session_id(&self) -> CaptureSessionId {
        self.session_id
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub const fn start_sample(&self) -> u64 {
        self.start_sample
    }

    pub const fn sample_count(&self) -> u64 {
        self.sample_count
    }

    pub fn end_sample(&self) -> u64 {
        self.start_sample + self.sample_count
    }

    pub fn channels(&self) -> &[CaptureChannelId] {
        &self.channels
    }

    pub fn payload(&self) -> &CaptureChunkPayload {
        &self.payload
    }

    pub fn encoded_byte_len(&self) -> usize {
        match &self.payload {
            CaptureChunkPayload::PackedLsbFirst { bytes, .. } => bytes.len(),
        }
    }

    pub fn packed_level(&self, relative_sample: u64, channel: usize) -> Option<bool> {
        if relative_sample >= self.sample_count || channel >= self.channels.len() {
            return None;
        }
        let relative_bit = (relative_sample as u128)
            .checked_mul(self.channels.len() as u128)?
            .checked_add(channel as u128)?;
        let CaptureChunkPayload::PackedLsbFirst { bytes, bit_offset } = &self.payload;
        let absolute_bit = relative_bit.checked_add(u128::from(*bit_offset))?;
        let byte_index = usize::try_from(absolute_bit / 8).ok()?;
        let bit_index = u8::try_from(absolute_bit % 8).ok()?;
        bytes
            .as_slice()
            .get(byte_index)
            .map(|byte| (byte & (1_u8 << bit_index)) != 0)
    }

    /// Returns the post-origin part of this chunk on a zero-based recording timeline.
    ///
    /// Chunks wholly after the origin retain their shared payload. Only the single chunk crossing
    /// the origin is repacked, keeping the common path allocation-free.
    pub fn recording_slice(
        &self,
        origin_sample: u64,
        output_sequence: u64,
    ) -> Result<Option<Self>, CaptureChunkError> {
        if self.end_sample() <= origin_sample {
            return Ok(None);
        }
        let skipped_samples = origin_sample.saturating_sub(self.start_sample());
        if skipped_samples == 0 {
            let mut chunk = self.clone();
            chunk.sequence = output_sequence;
            chunk.start_sample = chunk
                .start_sample
                .checked_sub(origin_sample)
                .ok_or(CaptureChunkError::SampleRangeOverflow)?;
            return Ok(Some(chunk));
        }

        let sample_count = self.sample_count - skipped_samples;
        let channel_count = self.channels.len();
        let bit_count = (sample_count as u128)
            .checked_mul(channel_count as u128)
            .ok_or(CaptureChunkError::PayloadSizeOverflow)?;
        let byte_count = usize::try_from(bit_count.div_ceil(8))
            .map_err(|_| CaptureChunkError::PayloadSizeOverflow)?;
        let mut bytes = vec![0_u8; byte_count];
        for sample in 0..sample_count {
            for channel in 0..channel_count {
                if self.packed_level(skipped_samples + sample, channel) == Some(true) {
                    let bit = sample as usize * channel_count + channel;
                    bytes[bit / 8] |= 1 << (bit % 8);
                }
            }
        }
        Self::packed_lsb_first(
            self.session_id,
            output_sequence,
            0,
            sample_count,
            Arc::clone(&self.channels),
            bytes,
            0,
        )
        .map(Some)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CaptureChunkError {
    #[error("a capture chunk must contain at least one channel")]
    NoChannels,
    #[error("a capture chunk must contain at least one sample")]
    NoSamples,
    #[error("capture chunk bit offset {0} is outside 0..8")]
    InvalidBitOffset(u8),
    #[error("capture chunk sample range overflows u64")]
    SampleRangeOverflow,
    #[error("capture chunk payload size overflows its representation")]
    PayloadSizeOverflow,
    #[error(
        "capture chunk payload has {available_bits} available bits but requires {required_bits}"
    )]
    PayloadTooShort {
        required_bits: u128,
        available_bits: u128,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureQueueLimits {
    max_queued_chunks: usize,
    max_chunk_bytes: usize,
}

impl CaptureQueueLimits {
    pub fn new(
        max_queued_chunks: usize,
        max_chunk_bytes: usize,
    ) -> Result<Self, CaptureQueueConfigError> {
        if max_queued_chunks == 0 {
            return Err(CaptureQueueConfigError::ZeroChunkCapacity);
        }
        if max_chunk_bytes == 0 {
            return Err(CaptureQueueConfigError::ZeroChunkSize);
        }
        Ok(Self {
            max_queued_chunks,
            max_chunk_bytes,
        })
    }

    pub const fn max_queued_chunks(self) -> usize {
        self.max_queued_chunks
    }

    pub const fn max_chunk_bytes(self) -> usize {
        self.max_chunk_bytes
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CaptureQueueConfigError {
    #[error("capture chunk queue capacity must be non-zero")]
    ZeroChunkCapacity,
    #[error("maximum capture chunk size must be non-zero")]
    ZeroChunkSize,
    #[error("capture event queue capacity must be non-zero")]
    ZeroEventCapacity,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CaptureWriteError {
    #[error("capture chunk queue is closed")]
    Closed,
    #[error("capture chunk contains {actual} bytes, exceeding the configured maximum of {limit}")]
    ChunkTooLarge { actual: usize, limit: usize },
    #[error("capture writer rejected the chunk: {0}")]
    Rejected(String),
}

/// Synchronous authority boundary used by an acquisition worker.
pub trait CaptureChunkWriter: Send {
    fn append(&mut self, chunk: CaptureChunk) -> Result<(), CaptureWriteError>;

    /// Makes every successfully appended chunk visible to committed readers.
    fn finish(&mut self) -> Result<(), CaptureWriteError> {
        Ok(())
    }
}

pub struct CaptureQueueWriter {
    sender: Sender<CaptureChunk>,
    limits: CaptureQueueLimits,
    max_observed: Arc<AtomicUsize>,
}

pub struct CaptureQueueReader {
    receiver: Receiver<CaptureChunk>,
    limits: CaptureQueueLimits,
    max_observed: Arc<AtomicUsize>,
}

pub fn bounded_capture_queue(
    limits: CaptureQueueLimits,
) -> (CaptureQueueWriter, CaptureQueueReader) {
    let (sender, receiver) = crossbeam_channel::bounded(limits.max_queued_chunks);
    let max_observed = Arc::new(AtomicUsize::new(0));
    (
        CaptureQueueWriter {
            sender,
            limits,
            max_observed: Arc::clone(&max_observed),
        },
        CaptureQueueReader {
            receiver,
            limits,
            max_observed,
        },
    )
}

impl CaptureChunkWriter for CaptureQueueWriter {
    fn append(&mut self, chunk: CaptureChunk) -> Result<(), CaptureWriteError> {
        let actual = chunk.encoded_byte_len();
        if actual > self.limits.max_chunk_bytes {
            return Err(CaptureWriteError::ChunkTooLarge {
                actual,
                limit: self.limits.max_chunk_bytes,
            });
        }
        self.sender
            .send(chunk)
            .map_err(|_| CaptureWriteError::Closed)?;
        self.max_observed
            .fetch_max(self.sender.len(), Ordering::Relaxed);
        Ok(())
    }
}

impl CaptureQueueReader {
    pub fn recv(&self) -> Result<CaptureChunk, CaptureQueueReceiveError> {
        self.receiver
            .recv()
            .map_err(|_| CaptureQueueReceiveError::Closed)
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<CaptureChunk, CaptureQueueReceiveError> {
        self.receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => CaptureQueueReceiveError::Timeout,
                RecvTimeoutError::Disconnected => CaptureQueueReceiveError::Closed,
            })
    }

    pub fn try_recv(&self) -> Result<CaptureChunk, CaptureQueueReceiveError> {
        self.receiver.try_recv().map_err(|error| match error {
            TryRecvError::Empty => CaptureQueueReceiveError::Empty,
            TryRecvError::Disconnected => CaptureQueueReceiveError::Closed,
        })
    }

    pub fn queued_chunks(&self) -> usize {
        self.receiver.len()
    }

    pub const fn capacity(&self) -> usize {
        self.limits.max_queued_chunks
    }

    pub fn max_observed_queued_chunks(&self) -> usize {
        self.max_observed.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CaptureQueueReceiveError {
    #[error("capture chunk queue is currently empty")]
    Empty,
    #[error("capture chunk receive timed out")]
    Timeout,
    #[error("capture chunk queue is closed")]
    Closed,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CaptureEventPublishError {
    #[error("capture event queue is full")]
    Full,
    #[error("capture event queue is closed")]
    Closed,
}

pub trait CaptureEventPublisher: Send {
    fn publish(&mut self, event: CaptureEvent) -> Result<(), CaptureEventPublishError>;
}

pub struct CaptureEventQueuePublisher {
    sender: Sender<CaptureEvent>,
}

pub struct CaptureEventQueueReader {
    receiver: Receiver<CaptureEvent>,
    capacity: usize,
}

pub fn bounded_capture_event_queue(
    capacity: usize,
) -> Result<(CaptureEventQueuePublisher, CaptureEventQueueReader), CaptureQueueConfigError> {
    if capacity == 0 {
        return Err(CaptureQueueConfigError::ZeroEventCapacity);
    }
    let (sender, receiver) = crossbeam_channel::bounded(capacity);
    Ok((
        CaptureEventQueuePublisher { sender },
        CaptureEventQueueReader { receiver, capacity },
    ))
}

impl CaptureEventPublisher for CaptureEventQueuePublisher {
    fn publish(&mut self, event: CaptureEvent) -> Result<(), CaptureEventPublishError> {
        self.sender.try_send(event).map_err(|error| match error {
            TrySendError::Full(_) => CaptureEventPublishError::Full,
            TrySendError::Disconnected(_) => CaptureEventPublishError::Closed,
        })
    }
}

impl CaptureEventQueueReader {
    pub fn recv(&self) -> Result<CaptureEvent, CaptureQueueReceiveError> {
        self.receiver
            .recv()
            .map_err(|_| CaptureQueueReceiveError::Closed)
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<CaptureEvent, CaptureQueueReceiveError> {
        self.receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => CaptureQueueReceiveError::Timeout,
                RecvTimeoutError::Disconnected => CaptureQueueReceiveError::Closed,
            })
    }

    pub fn try_recv(&self) -> Result<CaptureEvent, CaptureQueueReceiveError> {
        self.receiver.try_recv().map_err(|error| match error {
            TryRecvError::Empty => CaptureQueueReceiveError::Empty,
            TryRecvError::Disconnected => CaptureQueueReceiveError::Closed,
        })
    }

    pub fn queued_events(&self) -> usize {
        self.receiver.len()
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        CaptureBufferPool, CaptureChannelId, CaptureChunk, CaptureChunkError, CaptureChunkWriter,
        CaptureDataDelivery, CaptureProviderCapabilities, CaptureQueueLimits, CaptureSessionId,
        CaptureSettingCombination, CaptureWriteError, SimpleTriggerCondition,
        bounded_capture_queue,
    };
    use crate::{
        TriggerEditorSchema, TriggerIdentifier, TriggerLogicOperator, TriggerProgram,
        TriggerValidationCode,
    };

    fn channels() -> Arc<[CaptureChannelId]> {
        vec![
            CaptureChannelId::new("bank-a:7"),
            CaptureChannelId::new("bank-c:2"),
            CaptureChannelId::new("aux:19"),
        ]
        .into()
    }

    #[test]
    fn packed_chunk_validates_and_reads_unaligned_payload() {
        let levels = [true, false, true, false, true, false, true, true, false];
        let mut bytes = vec![0_u8; 2];
        for (relative, level) in levels.into_iter().enumerate() {
            if level {
                let bit = relative + 3;
                bytes[bit / 8] |= 1 << (bit % 8);
            }
        }
        let chunk = CaptureChunk::packed_lsb_first(
            CaptureSessionId::new(9),
            4,
            11,
            3,
            channels(),
            bytes,
            3,
        )
        .unwrap();

        assert_eq!(chunk.start_sample(), 11);
        assert_eq!(chunk.end_sample(), 14);
        assert_eq!(chunk.channels()[1].as_str(), "bank-c:2");
        assert_eq!(chunk.packed_level(0, 0), Some(true));
        assert_eq!(chunk.packed_level(1, 1), Some(true));
        assert_eq!(chunk.packed_level(2, 1), Some(true));
        assert_eq!(chunk.packed_level(3, 0), None);
        assert_eq!(chunk.packed_level(0, 3), None);
    }

    #[test]
    fn provider_capabilities_validate_and_match_only_explicit_setting_tuples() {
        let all_channels = channels();
        let bank_subset = vec![all_channels[0].clone(), all_channels[2].clone()];
        let capabilities = CaptureProviderCapabilities::new(
            CaptureDataDelivery::BufferedUpload,
            vec![
                CaptureSettingCombination::new(
                    Arc::clone(&all_channels),
                    Arc::from([1_000_000_u64]),
                )
                .unwrap(),
                CaptureSettingCombination::new(
                    bank_subset.clone(),
                    Arc::from([4_000_000_u64, 8_000_000]),
                )
                .unwrap(),
            ],
            false,
        )
        .unwrap();

        assert_eq!(
            capabilities.data_delivery(),
            CaptureDataDelivery::BufferedUpload
        );
        assert!(capabilities.supports(&all_channels, 1_000_000.0));
        assert!(capabilities.supports(&bank_subset, 8_000_000.0));
        assert!(!capabilities.supports(&all_channels, 8_000_000.0));
        assert!(!capabilities.supports(&bank_subset, 1_000_000.0));
        assert!(!capabilities.supports_force_trigger());
        assert!(capabilities.commands().orderly_stop);
        assert!(!capabilities.commands().abort);
        assert!(capabilities.commands().capture_now);
        assert!(!capabilities.policy().recording_starts().is_empty());

        assert!(CaptureSettingCombination::new(Vec::new(), Arc::from([1_u64])).is_err());
        assert!(
            CaptureSettingCombination::new(
                vec![all_channels[0].clone(), all_channels[0].clone()],
                Arc::from([1_u64]),
            )
            .is_err()
        );
        assert!(
            CaptureSettingCombination::new(Arc::clone(&all_channels), Arc::from([0_u64])).is_err()
        );
        assert!(
            CaptureProviderCapabilities::new(
                CaptureDataDelivery::DuringAcquisition,
                Vec::new(),
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn provider_capabilities_negotiate_only_advertised_trigger_programs() {
        let channels = channels();
        let schema = TriggerEditorSchema::new(
            TriggerIdentifier::new("test.capture-trigger").unwrap(),
            2,
            1,
            3,
            vec![TriggerLogicOperator::And],
        )
        .unwrap()
        .with_digital_conditions(vec![SimpleTriggerCondition::Rising])
        .unwrap();
        let program = schema
            .simple_program([(channels[1].clone(), SimpleTriggerCondition::Rising)])
            .unwrap()
            .unwrap();
        let capabilities = CaptureProviderCapabilities::single(
            CaptureDataDelivery::DuringAcquisition,
            Arc::clone(&channels),
            1_000_000,
        )
        .with_trigger_schema(schema);

        assert!(
            capabilities
                .negotiate_trigger_program(None, &channels)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            capabilities
                .negotiate_trigger_program(Some(&program), &channels)
                .unwrap()
                .unwrap()
                .program(),
            &program
        );

        let without_schema = CaptureProviderCapabilities::single(
            CaptureDataDelivery::DuringAcquisition,
            Arc::clone(&channels),
            1_000_000,
        );
        let error = without_schema
            .negotiate_trigger_program(Some(&program), &channels)
            .unwrap_err();
        assert_eq!(
            error.diagnostics()[0].code,
            TriggerValidationCode::SchemaUnavailable
        );

        let wrong_schema = TriggerProgram::new(
            TriggerIdentifier::new("test.other-trigger").unwrap(),
            2,
            program.stages.clone(),
        );
        let error = capabilities
            .negotiate_trigger_program(Some(&wrong_schema), &channels)
            .unwrap_err();
        assert!(
            error
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == TriggerValidationCode::SchemaIdentity)
        );
    }

    #[test]
    fn packed_chunk_rejects_short_payload() {
        let error = CaptureChunk::packed_lsb_first(
            CaptureSessionId::new(1),
            0,
            0,
            3,
            channels(),
            [0_u8],
            0,
        )
        .unwrap_err();
        assert_eq!(
            error,
            CaptureChunkError::PayloadTooShort {
                required_bits: 9,
                available_bits: 8,
            }
        );
    }

    #[test]
    fn bounded_queue_rejects_oversized_chunks() {
        let limits = CaptureQueueLimits::new(2, 1).unwrap();
        let (mut writer, reader) = bounded_capture_queue(limits);
        let chunk = CaptureChunk::packed_lsb_first(
            CaptureSessionId::new(1),
            0,
            0,
            3,
            channels(),
            [0_u8; 2],
            0,
        )
        .unwrap();

        assert_eq!(
            writer.append(chunk),
            Err(CaptureWriteError::ChunkTooLarge {
                actual: 2,
                limit: 1,
            })
        );
        assert_eq!(reader.queued_chunks(), 0);
        assert_eq!(reader.capacity(), 2);
    }

    #[test]
    fn buffer_pool_reuses_allocation_after_last_chunk_owner_drops() {
        let pool = CaptureBufferPool::new(1, 64).unwrap();
        let mut lease = pool.acquire();
        lease.resize(17, 0xaa);
        let bytes = lease.freeze();
        assert_eq!(bytes.len(), 17);
        assert_eq!(pool.metrics().in_use, 1);
        drop(bytes);

        let mut second = pool.acquire();
        second.resize(9, 0x55);
        assert_eq!(second.as_mut_slice(), &[0x55; 9]);
        drop(second);
        let metrics = pool.metrics();
        assert_eq!(metrics.allocated, 1);
        assert_eq!(metrics.available, 1);
        assert_eq!(metrics.in_use, 0);
        assert_eq!(metrics.max_in_use, 1);
    }

    #[test]
    fn simple_trigger_condition_truth_table_covers_levels_and_edges() {
        use SimpleTriggerCondition::{Either, Falling, High, Ignore, Low, Rising};

        assert!(Ignore.matches(None, false));
        assert!(Ignore.matches(Some(true), false));
        assert!(Low.matches(None, false));
        assert!(!Low.matches(None, true));
        assert!(High.matches(None, true));
        assert!(!High.matches(None, false));
        assert!(Rising.matches(Some(false), true));
        assert!(!Rising.matches(None, true));
        assert!(!Rising.matches(Some(true), true));
        assert!(Falling.matches(Some(true), false));
        assert!(!Falling.matches(None, false));
        assert!(Either.matches(Some(false), true));
        assert!(Either.matches(Some(true), false));
        assert!(!Either.matches(Some(true), true));
        assert!(!Either.matches(None, true));
    }

    #[test]
    fn recording_slice_rebases_and_repackages_only_the_crossing_chunk() {
        let levels = [true, true, false, true, true, false];
        let chunk = CaptureChunk::packed_lsb_first(
            CaptureSessionId::new(4),
            8,
            10,
            2,
            channels(),
            [0b0011_0110_u8],
            1,
        )
        .unwrap();

        let sliced = chunk.recording_slice(11, 0).unwrap().unwrap();
        assert_eq!(sliced.sequence(), 0);
        assert_eq!(sliced.start_sample(), 0);
        assert_eq!(sliced.sample_count(), 1);
        for channel in 0..3 {
            assert_eq!(sliced.packed_level(0, channel), Some(levels[3 + channel]));
        }

        let rebased = chunk.recording_slice(5, 2).unwrap().unwrap();
        assert_eq!(rebased.sequence(), 2);
        assert_eq!(rebased.start_sample(), 5);
        assert_eq!(rebased.payload(), chunk.payload());
        assert!(chunk.recording_slice(12, 0).unwrap().is_none());
    }
}
