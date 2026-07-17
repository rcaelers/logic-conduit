//! Deterministic native provider used by live-capture contract and integration tests.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use signal_processing::{
    CaptureAcquisitionPhase, CaptureChannelId, CaptureChunk, CaptureProgress, CaptureSessionId,
    CaptureSessionState,
};

use super::{
    AcquisitionContext, AcquisitionError, AcquisitionOutcome, AcquisitionResult,
    PreparedAcquisition,
};

#[derive(Clone, Debug)]
pub struct DeterministicFakeConfig {
    channels: Arc<[CaptureChannelId]>,
    chunk_sample_counts: Arc<[u64]>,
    seed: u64,
}

impl DeterministicFakeConfig {
    pub fn new(
        channels: impl Into<Arc<[CaptureChannelId]>>,
        chunk_sample_counts: impl Into<Arc<[u64]>>,
        seed: u64,
    ) -> AcquisitionResult<Self> {
        let channels = channels.into();
        let chunk_sample_counts = chunk_sample_counts.into();
        if channels.is_empty() {
            return Err(AcquisitionError::InvalidRequest(
                "fake capture requires at least one channel".into(),
            ));
        }
        if chunk_sample_counts.is_empty() || chunk_sample_counts.contains(&0) {
            return Err(AcquisitionError::InvalidRequest(
                "fake capture requires non-empty, non-zero chunk sizes".into(),
            ));
        }
        chunk_sample_counts.iter().try_fold(0_u64, |total, count| {
            total.checked_add(*count).ok_or_else(|| {
                AcquisitionError::InvalidRequest("fake capture sample count overflows u64".into())
            })
        })?;
        Ok(Self {
            channels,
            chunk_sample_counts,
            seed,
        })
    }

    pub fn channels(&self) -> &[CaptureChannelId] {
        &self.channels
    }

    pub fn chunk_sample_counts(&self) -> &[u64] {
        &self.chunk_sample_counts
    }

    pub fn total_samples(&self) -> u64 {
        self.chunk_sample_counts.iter().sum()
    }

    pub fn level_at(&self, sample: u64, channel: usize) -> bool {
        let channel = channel as u64;
        let mixed = sample
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .rotate_left((channel % 63) as u32)
            ^ channel.wrapping_mul(0xd6e8_feb8_6659_fd93)
            ^ self.seed;
        (mixed ^ (mixed >> 17) ^ (mixed >> 41)) & 1 != 0
    }

    fn build_chunk(
        &self,
        session_id: CaptureSessionId,
        sequence: u64,
        start_sample: u64,
        sample_count: u64,
    ) -> AcquisitionResult<CaptureChunk> {
        let bit_offset = ((sequence * 3 + 1) % 8) as u8;
        let payload_bits = (sample_count as u128)
            .checked_mul(self.channels.len() as u128)
            .ok_or_else(|| AcquisitionError::Internal("fake payload size overflow".into()))?;
        let total_bits = payload_bits + u128::from(bit_offset);
        let byte_len = usize::try_from(total_bits.div_ceil(8))
            .map_err(|_| AcquisitionError::Internal("fake payload is too large".into()))?;
        let mut bytes = vec![0_u8; byte_len];
        for relative_sample in 0..sample_count {
            for channel in 0..self.channels.len() {
                if self.level_at(start_sample + relative_sample, channel) {
                    let relative_bit = relative_sample as usize * self.channels.len() + channel;
                    let absolute_bit = usize::from(bit_offset) + relative_bit;
                    bytes[absolute_bit / 8] |= 1 << (absolute_bit % 8);
                }
            }
        }
        CaptureChunk::packed_lsb_first(
            session_id,
            sequence,
            start_sample,
            sample_count,
            Arc::clone(&self.channels),
            bytes,
            bit_offset,
        )
        .map_err(|error| AcquisitionError::Internal(error.to_string()))
    }
}

#[derive(Debug)]
struct FakeControlState {
    manual: bool,
    permits: usize,
    stop_requested: bool,
}

#[derive(Debug)]
struct FakeControl {
    state: Mutex<FakeControlState>,
    changed: Condvar,
}

impl FakeControl {
    fn new(manual: bool) -> Self {
        Self {
            state: Mutex::new(FakeControlState {
                manual,
                permits: 0,
                stop_requested: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn wait_for_chunk(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        while state.manual && state.permits == 0 && !state.stop_requested {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        if state.stop_requested {
            return false;
        }
        if state.manual {
            state.permits -= 1;
        }
        true
    }

    fn grant(&self, chunks: usize) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.permits = state.permits.saturating_add(chunks);
        self.changed.notify_all();
    }

    fn request_stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.stop_requested = true;
        self.changed.notify_all();
    }
}

#[derive(Clone, Debug)]
pub struct DeterministicFakeController {
    control: Arc<FakeControl>,
}

impl DeterministicFakeController {
    pub fn grant_chunks(&self, chunks: usize) {
        self.control.grant(chunks);
    }
}

pub struct DeterministicFakeProvider {
    config: DeterministicFakeConfig,
    control: Arc<FakeControl>,
}

impl DeterministicFakeProvider {
    pub fn new(config: DeterministicFakeConfig) -> Self {
        Self {
            config,
            control: Arc::new(FakeControl::new(false)),
        }
    }

    pub fn manually_paced(
        config: DeterministicFakeConfig,
    ) -> (Self, DeterministicFakeController) {
        let control = Arc::new(FakeControl::new(true));
        (
            Self {
                config,
                control: Arc::clone(&control),
            },
            DeterministicFakeController { control },
        )
    }

    pub fn prepare(
        self,
        mut context: AcquisitionContext,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        context.publish_status(
            CaptureSessionState::Preparing,
            CaptureAcquisitionPhase::Preparing,
        )?;
        context.publish_status(
            CaptureSessionState::Prepared,
            CaptureAcquisitionPhase::Ready,
        )?;
        Ok(Box::new(PreparedFakeAcquisition {
            session_id: context.session_id(),
            context: Some(context),
            config: self.config,
            control: self.control,
            handle: None,
            started: false,
        }))
    }
}

struct PreparedFakeAcquisition {
    session_id: CaptureSessionId,
    context: Option<AcquisitionContext>,
    config: DeterministicFakeConfig,
    control: Arc<FakeControl>,
    handle: Option<JoinHandle<AcquisitionResult<AcquisitionOutcome>>>,
    started: bool,
}

impl PreparedFakeAcquisition {
    fn run(
        mut context: AcquisitionContext,
        config: DeterministicFakeConfig,
        control: Arc<FakeControl>,
    ) -> AcquisitionResult<AcquisitionOutcome> {
        let result = Self::run_inner(&mut context, &config, &control);
        if let Err(error) = &result {
            context.publish_failure(error);
        }
        result
    }

    fn run_inner(
        context: &mut AcquisitionContext,
        config: &DeterministicFakeConfig,
        control: &FakeControl,
    ) -> AcquisitionResult<AcquisitionOutcome> {
        context.publish_status(
            CaptureSessionState::Recording,
            CaptureAcquisitionPhase::ReceivingLiveData,
        )?;
        let mut captured_samples = 0_u64;
        let mut transferred_bytes = 0_u64;
        let mut chunk_count = 0_u64;
        let mut stopped = false;
        for (sequence, sample_count) in config.chunk_sample_counts.iter().copied().enumerate() {
            if !control.wait_for_chunk() {
                stopped = true;
                break;
            }
            let chunk = config.build_chunk(
                context.session_id(),
                sequence as u64,
                captured_samples,
                sample_count,
            )?;
            transferred_bytes = transferred_bytes
                .checked_add(chunk.encoded_byte_len() as u64)
                .ok_or_else(|| AcquisitionError::Internal("byte count overflow".into()))?;
            context.append(chunk)?;
            captured_samples += sample_count;
            chunk_count += 1;
            context.publish_progress(CaptureProgress {
                captured_samples: Some(captured_samples),
                transferred_bytes: Some(transferred_bytes),
            })?;
        }
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
            chunk_count,
            stopped,
        })
    }

    fn join_worker(&mut self) -> AcquisitionResult<AcquisitionOutcome> {
        let handle = self.handle.take().ok_or(AcquisitionError::NotStarted)?;
        handle.join().map_err(|_| AcquisitionError::WorkerPanicked)?
    }
}

impl PreparedAcquisition for PreparedFakeAcquisition {
    fn session_id(&self) -> CaptureSessionId {
        self.session_id
    }

    fn start(&mut self) -> AcquisitionResult<()> {
        if self.started {
            return Err(AcquisitionError::AlreadyStarted);
        }
        let context = self.context.take().ok_or(AcquisitionError::AlreadyStarted)?;
        let config = self.config.clone();
        let control = Arc::clone(&self.control);
        self.handle = Some(
            std::thread::Builder::new()
                .name("deterministic-live-capture".into())
                .spawn(move || Self::run(context, config, control))
                .map_err(|error| AcquisitionError::WorkerStart(error.to_string()))?,
        );
        self.started = true;
        Ok(())
    }

    fn request_stop(&self) -> AcquisitionResult<()> {
        self.control.request_stop();
        Ok(())
    }

    fn join(mut self: Box<Self>) -> AcquisitionResult<AcquisitionOutcome> {
        self.join_worker()
    }
}

impl Drop for PreparedFakeAcquisition {
    fn drop(&mut self) {
        self.control.request_stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use signal_processing::{
        CaptureChannelId, CaptureEvent, CaptureQueueLimits, CaptureQueueReceiveError,
        CaptureSessionId, CaptureSessionState, bounded_capture_event_queue, bounded_capture_queue,
    };

    use super::{DeterministicFakeConfig, DeterministicFakeProvider};
    use crate::live_capture::{AcquisitionContext, AcquisitionError};

    const TIMEOUT: Duration = Duration::from_secs(2);

    fn config() -> DeterministicFakeConfig {
        DeterministicFakeConfig::new(
            vec![
                CaptureChannelId::new("bank-a:7"),
                CaptureChannelId::new("bank-c:2"),
                CaptureChannelId::new("aux:19"),
            ],
            vec![3, 5, 2, 7, 4],
            0x5a17,
        )
        .unwrap()
    }

    #[test]
    fn lifecycle_reconstructs_exact_samples_with_bounded_queues() {
        let config = config();
        let session_id = CaptureSessionId::new(0x1234);
        let limits = CaptureQueueLimits::new(2, 16).unwrap();
        let (writer, chunks) = bounded_capture_queue(limits);
        let (events, event_reader) = bounded_capture_event_queue(32).unwrap();
        let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
        let mut acquisition = DeterministicFakeProvider::new(config.clone())
            .prepare(context)
            .unwrap();

        assert_eq!(acquisition.session_id(), session_id);
        acquisition.start().unwrap();
        assert_eq!(
            acquisition.start(),
            Err(AcquisitionError::AlreadyStarted)
        );
        let mut received = Vec::new();
        loop {
            match chunks.recv_timeout(TIMEOUT) {
                Ok(chunk) => received.push(chunk),
                Err(CaptureQueueReceiveError::Closed) => break,
                Err(error) => panic!("unexpected chunk receive error: {error}"),
            }
        }
        acquisition.request_stop().unwrap();
        acquisition.request_stop().unwrap();
        let outcome = acquisition.join().unwrap();

        assert_eq!(outcome.captured_samples, config.total_samples());
        assert_eq!(outcome.chunk_count as usize, config.chunk_sample_counts().len());
        assert!(!outcome.stopped);
        assert!(chunks.max_observed_queued_chunks() <= chunks.capacity());
        for (sequence, chunk) in received.iter().enumerate() {
            assert_eq!(chunk.sequence(), sequence as u64);
            for sample in 0..chunk.sample_count() {
                for channel in 0..config.channels().len() {
                    assert_eq!(
                        chunk.packed_level(sample, channel),
                        Some(config.level_at(chunk.start_sample() + sample, channel))
                    );
                }
            }
        }

        let mut states = Vec::new();
        loop {
            match event_reader.recv_timeout(TIMEOUT) {
                Ok(CaptureEvent::Status(status)) => states.push(status.state),
                Ok(CaptureEvent::Progress { .. }) => {}
                Ok(CaptureEvent::Failed(failure)) => panic!("unexpected failure: {failure:?}"),
                Err(CaptureQueueReceiveError::Closed) => break,
                Err(error) => panic!("unexpected event receive error: {error}"),
            }
        }
        assert_eq!(
            states,
            [
                CaptureSessionState::Preparing,
                CaptureSessionState::Prepared,
                CaptureSessionState::Recording,
                CaptureSessionState::Stopping,
                CaptureSessionState::Complete,
            ]
        );
        assert!(event_reader.queued_events() <= event_reader.capacity());
    }

    #[test]
    fn manual_stop_is_idempotent_and_finishes_at_an_exact_chunk_boundary() {
        let config = config();
        let session_id = CaptureSessionId::new(0x5678);
        let limits = CaptureQueueLimits::new(1, 16).unwrap();
        let (writer, chunks) = bounded_capture_queue(limits);
        let (events, _event_reader) = bounded_capture_event_queue(32).unwrap();
        let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
        let (provider, controller) = DeterministicFakeProvider::manually_paced(config.clone());
        let mut acquisition = provider.prepare(context).unwrap();

        acquisition.start().unwrap();
        controller.grant_chunks(2);
        let first = chunks.recv_timeout(TIMEOUT).unwrap();
        let second = chunks.recv_timeout(TIMEOUT).unwrap();
        acquisition.request_stop().unwrap();
        acquisition.request_stop().unwrap();
        let outcome = acquisition.join().unwrap();

        assert_eq!(first.sequence(), 0);
        assert_eq!(second.sequence(), 1);
        assert_eq!(outcome.chunk_count, 2);
        assert_eq!(outcome.captured_samples, 8);
        assert!(outcome.stopped);
        assert_eq!(
            chunks.recv_timeout(TIMEOUT),
            Err(CaptureQueueReceiveError::Closed)
        );
        assert!(chunks.max_observed_queued_chunks() <= 1);
    }
}
