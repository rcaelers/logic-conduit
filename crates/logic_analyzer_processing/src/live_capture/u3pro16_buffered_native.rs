//! Device-buffered live-acquisition adapter for the concrete U3Pro16 driver.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use signal_processing::{
    CaptureAcquisitionPhase, CaptureChannelId, CaptureChunk, CaptureProgress,
    CaptureSessionId, CaptureSessionState,
};

use crate::nodes::{
    DsLogicCapturePlan, DsLogicU3Pro16, LogicAnalyzer, LogicAnalyzerError,
    LogicCaptureConfig, LogicEncoding, RusbTransport, UsbTransport,
};

use super::{
    AcquisitionContext, AcquisitionError, AcquisitionOutcome, AcquisitionResult,
    PreparedAcquisition,
};

pub struct DsLogicU3Pro16BufferedProvider<T: UsbTransport = RusbTransport> {
    analyzer: DsLogicU3Pro16<T>,
    config: LogicCaptureConfig,
    channels: Arc<[CaptureChannelId]>,
}

impl DsLogicU3Pro16BufferedProvider<RusbTransport> {
    pub fn open_first(
        config: LogicCaptureConfig,
        channels: impl Into<Arc<[CaptureChannelId]>>,
    ) -> AcquisitionResult<Self> {
        let analyzer = DsLogicU3Pro16::open_first().map_err(map_analyzer_error)?;
        Self::new(analyzer, config, channels)
    }
}

impl<T: UsbTransport> DsLogicU3Pro16BufferedProvider<T> {
    pub fn new(
        analyzer: DsLogicU3Pro16<T>,
        config: LogicCaptureConfig,
        channels: impl Into<Arc<[CaptureChannelId]>>,
    ) -> AcquisitionResult<Self> {
        let channels = channels.into();
        if channels.is_empty() || channels.len() != config.input_mask.count_ones() as usize {
            return Err(AcquisitionError::InvalidRequest(
                "U3Pro16 channel identities must match the enabled physical inputs".into(),
            ));
        }
        Ok(Self {
            analyzer,
            config,
            channels,
        })
    }

    pub fn prepare(
        mut self,
        mut context: AcquisitionContext,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        context.publish_status(
            CaptureSessionState::Preparing,
            CaptureAcquisitionPhase::Preparing,
        )?;
        let plan = self
            .analyzer
            .prepare_buffered_capture(&self.config)
            .map_err(map_analyzer_error)?;
        if usize::from(plan.channel_count()) != self.channels.len() {
            return Err(AcquisitionError::Protocol(
                "negotiated U3Pro16 channel count changed unexpectedly".into(),
            ));
        }
        context.publish_status(
            CaptureSessionState::Prepared,
            CaptureAcquisitionPhase::Ready,
        )?;
        Ok(Box::new(PreparedBufferedAcquisition {
            session_id: context.session_id(),
            context: Some(context),
            analyzer: Some(self.analyzer),
            config: self.config,
            channels: self.channels,
            plan,
            stop_requested: Arc::new(AtomicBool::new(false)),
            handle: None,
            started: false,
        }))
    }
}

struct PreparedBufferedAcquisition<T: UsbTransport> {
    session_id: CaptureSessionId,
    context: Option<AcquisitionContext>,
    analyzer: Option<DsLogicU3Pro16<T>>,
    config: LogicCaptureConfig,
    channels: Arc<[CaptureChannelId]>,
    plan: DsLogicCapturePlan,
    stop_requested: Arc<AtomicBool>,
    handle: Option<JoinHandle<AcquisitionResult<AcquisitionOutcome>>>,
    started: bool,
}

impl<T: UsbTransport> PreparedBufferedAcquisition<T> {
    fn run(
        mut context: AcquisitionContext,
        mut analyzer: DsLogicU3Pro16<T>,
        config: LogicCaptureConfig,
        channels: Arc<[CaptureChannelId]>,
        plan: DsLogicCapturePlan,
        stop_requested: Arc<AtomicBool>,
    ) -> AcquisitionResult<AcquisitionOutcome> {
        let result = Self::run_inner(
            &mut context,
            &mut analyzer,
            &config,
            &channels,
            plan,
            &stop_requested,
        );
        if let Err(error) = &result {
            context.publish_failure(error);
        }
        result
    }

    fn run_inner(
        context: &mut AcquisitionContext,
        analyzer: &mut DsLogicU3Pro16<T>,
        config: &LogicCaptureConfig,
        channels: &Arc<[CaptureChannelId]>,
        plan: DsLogicCapturePlan,
        stop_requested: &AtomicBool,
    ) -> AcquisitionResult<AcquisitionOutcome> {
        let armed = !config.trigger.stages.is_empty();
        context.publish_status(
            if armed {
                CaptureSessionState::Armed
            } else {
                CaptureSessionState::Recording
            },
            if armed {
                CaptureAcquisitionPhase::WaitingForTrigger
            } else {
                CaptureAcquisitionPhase::CapturingOnDevice
            },
        )?;
        analyzer.start_capture().map_err(map_analyzer_error)?;

        let mut header_seen = false;
        let mut expected_samples = None;
        let mut captured_samples = 0_u64;
        let mut transferred_bytes = 0_u64;
        let mut input_bits = 0_u64;
        let mut sequence = 0_u64;
        let mut carry = Vec::new();
        let mut stopped = false;
        loop {
            if stop_requested.load(Ordering::Relaxed) {
                stopped = true;
                break;
            }
            let next = match analyzer.next_chunk() {
                Ok(next) => next,
                Err(_error) if stop_requested.load(Ordering::Relaxed) => {
                    stopped = true;
                    break;
                }
                Err(error) => return Err(map_analyzer_error(error)),
            };
            if !header_seen
                && let Some(header) = analyzer.take_trigger_header()
            {
                header_seen = true;
                expected_samples = Some(header.captured_samples());
                if let Some(trigger_sample) = header.trigger_sample() {
                    context.publish_triggered(trigger_sample)?;
                    context.publish_status(
                        CaptureSessionState::Triggered,
                        CaptureAcquisitionPhase::CapturingOnDevice,
                    )?;
                }
                context.publish_status(
                    CaptureSessionState::Recording,
                    CaptureAcquisitionPhase::UploadingBufferedData,
                )?;
            }

            let Some(chunk) = next else {
                break;
            };
            if chunk.bit_len == 0 {
                continue;
            }
            if !header_seen {
                return Err(AcquisitionError::Protocol(
                    "U3Pro16 data arrived before its trigger header".into(),
                ));
            }
            if chunk.encoding != LogicEncoding::InterleavedLsbFirst
                || usize::from(chunk.channel_count) != channels.len()
            {
                return Err(AcquisitionError::Protocol(
                    "U3Pro16 upload encoding or channel table changed unexpectedly".into(),
                ));
            }
            let channel_count = u64::from(chunk.channel_count);
            if chunk.start_bit != input_bits {
                return Err(AcquisitionError::Protocol(format!(
                    "U3Pro16 upload starts at bit {}, expected {input_bits}",
                    chunk.start_bit
                )));
            }
            input_bits = input_bits
                .checked_add(chunk.bit_len as u64)
                .ok_or_else(|| AcquisitionError::Protocol("upload bit count overflow".into()))?;
            let (bytes, sample_count, next_carry) =
                canonicalize_transfer(&carry, &chunk, channel_count as usize)?;
            carry = next_carry;
            if sample_count == 0 {
                continue;
            }
            let canonical = CaptureChunk::packed_lsb_first(
                context.session_id(),
                sequence,
                captured_samples,
                sample_count,
                Arc::clone(channels),
                bytes,
                0,
            )
            .map_err(|error| AcquisitionError::Protocol(error.to_string()))?;
            transferred_bytes = transferred_bytes
                .checked_add(canonical.encoded_byte_len() as u64)
                .ok_or_else(|| AcquisitionError::Protocol("upload byte count overflow".into()))?;
            context.append(canonical)?;
            captured_samples += sample_count;
            sequence += 1;
            context.publish_progress(CaptureProgress {
                captured_samples: Some(captured_samples),
                transferred_bytes: Some(transferred_bytes),
            })?;
        }
        analyzer.stop_capture().map_err(map_analyzer_error)?;
        if !stopped && !header_seen {
            return Err(AcquisitionError::Protocol(
                "U3Pro16 capture ended without a trigger header".into(),
            ));
        }
        if !stopped
            && let Some(expected_samples) = expected_samples
            && captured_samples != expected_samples
        {
            return Err(AcquisitionError::Protocol(format!(
                "U3Pro16 uploaded {captured_samples} samples, header promised {expected_samples}"
            )));
        }
        if !stopped && !carry.is_empty() {
            return Err(AcquisitionError::Protocol(format!(
                "U3Pro16 upload ended with {} bits of an incomplete sample",
                carry.len()
            )));
        }
        if captured_samples > plan.actual_samples() {
            return Err(AcquisitionError::Protocol(
                "U3Pro16 uploaded more samples than its immutable plan".into(),
            ));
        }
        context.finish_writer()?;
        context.publish_status(
            CaptureSessionState::Stopping,
            CaptureAcquisitionPhase::Finalizing,
        )?;
        context.publish_status(
            CaptureSessionState::Complete,
            CaptureAcquisitionPhase::Finalizing,
        )?;
        Ok(AcquisitionOutcome {
            session_id: context.session_id(),
            captured_samples,
            chunk_count: sequence,
            stopped,
        })
    }

    fn join_worker(&mut self) -> AcquisitionResult<AcquisitionOutcome> {
        let handle = self.handle.take().ok_or(AcquisitionError::NotStarted)?;
        handle.join().map_err(|_| AcquisitionError::WorkerPanicked)?
    }
}

fn canonicalize_transfer(
    carry: &[bool],
    chunk: &crate::nodes::LogicChunk,
    channel_count: usize,
) -> AcquisitionResult<(Vec<u8>, u64, Vec<bool>)> {
    if channel_count == 0 {
        return Err(AcquisitionError::Protocol(
            "U3Pro16 upload has no channels".into(),
        ));
    }
    let bit_offset = usize::from(chunk.bit_offset);
    let available_bits = chunk
        .data
        .len()
        .checked_mul(8)
        .and_then(|bits| bits.checked_sub(bit_offset))
        .ok_or_else(|| AcquisitionError::Protocol("invalid upload bit span".into()))?;
    if chunk.bit_len > available_bits {
        return Err(AcquisitionError::Protocol(
            "U3Pro16 upload bit span exceeds its transfer buffer".into(),
        ));
    }
    let total_bits = carry
        .len()
        .checked_add(chunk.bit_len)
        .ok_or_else(|| AcquisitionError::Protocol("upload bit count overflow".into()))?;
    let sample_count = total_bits / channel_count;
    let complete_bits = sample_count * channel_count;
    if complete_bits == 0 {
        let mut next_carry = carry.to_vec();
        next_carry.extend((0..chunk.bit_len).map(|bit| chunk.bit(bit)));
        return Ok((Vec::new(), 0, next_carry));
    }

    let mut bytes = vec![0_u8; complete_bits.div_ceil(8)];
    for (bit, level) in carry.iter().copied().enumerate() {
        if level {
            bytes[bit / 8] |= 1 << (bit % 8);
        }
    }

    let source_bits = complete_bits - carry.len();
    let destination_shift = carry.len() % 8;
    for source_byte in 0..source_bits.div_ceil(8) {
        let source_bit = source_byte * 8;
        let absolute_bit = bit_offset + source_bit;
        let data_byte = absolute_bit / 8;
        let source_shift = absolute_bit % 8;
        let mut value = chunk.data[data_byte] >> source_shift;
        if source_shift != 0
            && let Some(next) = chunk.data.get(data_byte + 1)
        {
            value |= *next << (8 - source_shift);
        }
        let destination_bit = carry.len() + source_bit;
        let destination_byte = destination_bit / 8;
        bytes[destination_byte] |= value << destination_shift;
        if destination_shift != 0
            && let Some(next) = bytes.get_mut(destination_byte + 1)
        {
            *next |= value >> (8 - destination_shift);
        }
    }

    if !complete_bits.is_multiple_of(8) {
        *bytes.last_mut().unwrap() &= (1 << (complete_bits % 8)) - 1;
    }
    let next_carry = (source_bits..chunk.bit_len)
        .map(|bit| chunk.bit(bit))
        .collect();
    Ok((bytes, sample_count as u64, next_carry))
}

impl<T: UsbTransport> PreparedAcquisition for PreparedBufferedAcquisition<T> {
    fn session_id(&self) -> CaptureSessionId {
        self.session_id
    }

    fn start(&mut self) -> AcquisitionResult<()> {
        if self.started {
            return Err(AcquisitionError::AlreadyStarted);
        }
        let context = self.context.take().ok_or(AcquisitionError::AlreadyStarted)?;
        let analyzer = self.analyzer.take().ok_or(AcquisitionError::AlreadyStarted)?;
        let config = self.config.clone();
        let channels = Arc::clone(&self.channels);
        let plan = self.plan;
        let stop_requested = Arc::clone(&self.stop_requested);
        self.handle = Some(
            std::thread::Builder::new()
                .name("u3pro16-buffered-capture".into())
                .spawn(move || {
                    Self::run(context, analyzer, config, channels, plan, stop_requested)
                })
                .map_err(|error| AcquisitionError::WorkerStart(error.to_string()))?,
        );
        self.started = true;
        Ok(())
    }

    fn request_stop(&self) -> AcquisitionResult<()> {
        self.stop_requested.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn is_finished(&self) -> bool {
        self.handle.as_ref().is_some_and(JoinHandle::is_finished)
    }

    fn join(mut self: Box<Self>) -> AcquisitionResult<AcquisitionOutcome> {
        self.join_worker()
    }
}

impl<T: UsbTransport> Drop for PreparedBufferedAcquisition<T> {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn map_analyzer_error(error: LogicAnalyzerError) -> AcquisitionError {
    match error {
        LogicAnalyzerError::InvalidSettings(message) => AcquisitionError::InvalidRequest(message),
        LogicAnalyzerError::Transport(message) | LogicAnalyzerError::Timeout(message) => {
            AcquisitionError::Transport(message)
        }
        LogicAnalyzerError::Protocol(message) => AcquisitionError::Protocol(message),
        LogicAnalyzerError::NotCapturing => {
            AcquisitionError::Protocol("U3Pro16 capture is not active".into())
        }
    }
}
