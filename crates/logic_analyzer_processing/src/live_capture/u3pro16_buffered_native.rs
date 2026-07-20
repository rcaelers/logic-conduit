//! Device-buffered live-acquisition adapter for the concrete U3Pro16 driver.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use signal_processing::{
    CaptureAcquisitionPhase, CaptureChannelId, CaptureChunk, CaptureCompletion, CaptureProgress,
    CaptureSessionId, CaptureSessionState,
};

use super::u3pro16_common_native::{CanonicalTransferAssembler, map_analyzer_error};
use super::{
    AcquisitionContext, AcquisitionError, AcquisitionOutcome, AcquisitionResult,
    PreparedAcquisition,
};
use crate::nodes::{
    DsLogicCapturePlan, DsLogicU3Pro16, LogicAnalyzer, LogicCaptureConfig, RusbTransport,
    UsbTransport,
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
        let mut sequence = 0_u64;
        let mut canonicalizer = CanonicalTransferAssembler::default();
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
            if !header_seen && let Some(header) = analyzer.take_trigger_header() {
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
            let Some(transfer) = canonicalizer.push(&chunk, channels.len())? else {
                continue;
            };
            let canonical = CaptureChunk::packed_lsb_first(
                context.session_id(),
                sequence,
                captured_samples,
                transfer.sample_count,
                Arc::clone(channels),
                transfer.bytes,
                0,
            )
            .map_err(|error| AcquisitionError::Protocol(error.to_string()))?;
            transferred_bytes = transferred_bytes
                .checked_add(canonical.encoded_byte_len() as u64)
                .ok_or_else(|| AcquisitionError::Protocol("upload byte count overflow".into()))?;
            context.append(canonical)?;
            captured_samples += transfer.sample_count;
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
        if !stopped {
            canonicalizer.finish()?;
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
            completion: if stopped && armed && !header_seen {
                CaptureCompletion::CancelledBeforeTrigger
            } else if stopped {
                CaptureCompletion::Stopped
            } else {
                CaptureCompletion::Finished
            },
        })
    }

    fn join_worker(&mut self) -> AcquisitionResult<AcquisitionOutcome> {
        let handle = self.handle.take().ok_or(AcquisitionError::NotStarted)?;
        handle
            .join()
            .map_err(|_| AcquisitionError::WorkerPanicked)?
    }
}

impl<T: UsbTransport> PreparedAcquisition for PreparedBufferedAcquisition<T> {
    fn session_id(&self) -> CaptureSessionId {
        self.session_id
    }

    fn start(&mut self) -> AcquisitionResult<()> {
        if self.started {
            return Err(AcquisitionError::AlreadyStarted);
        }
        let context = self
            .context
            .take()
            .ok_or(AcquisitionError::AlreadyStarted)?;
        let analyzer = self
            .analyzer
            .take()
            .ok_or(AcquisitionError::AlreadyStarted)?;
        let config = self.config.clone();
        let channels = Arc::clone(&self.channels);
        let plan = self.plan;
        let stop_requested = Arc::clone(&self.stop_requested);
        self.handle = Some(
            std::thread::Builder::new()
                .name("u3pro16-buffered-capture".into())
                .spawn(move || Self::run(context, analyzer, config, channels, plan, stop_requested))
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
