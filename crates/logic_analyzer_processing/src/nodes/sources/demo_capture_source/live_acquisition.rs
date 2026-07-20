//! Deterministic native provider used by live-capture contract and integration tests.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use signal_processing::{
    AcquisitionContext, AcquisitionError, AcquisitionOutcome, AcquisitionResult,
    CaptureAcquisitionPhase, CaptureBufferPool, CaptureChannelId, CaptureChunk, CaptureCompletion,
    CaptureProgress, CaptureSessionId, CaptureSessionState, PreparedAcquisition,
    SimpleTriggerCondition,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeterministicTriggerLogic {
    And,
    Or,
    Xor,
    Nand,
    Nor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeterministicTriggerCountMode {
    Occurrences,
    Consecutive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeterministicTriggerCount {
    pub mode: DeterministicTriggerCountMode,
    pub value: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicTriggerPredicate {
    pub channel: usize,
    pub condition: SimpleTriggerCondition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicTriggerStage {
    pub predicates: Vec<DeterministicTriggerPredicate>,
    pub logic: DeterministicTriggerLogic,
    pub inverted: bool,
    pub count: Option<DeterministicTriggerCount>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicTrigger {
    pub stages: Vec<DeterministicTriggerStage>,
}

#[derive(Clone, Debug, Default)]
struct DeterministicTriggerEvaluator {
    stage: usize,
    matched_count: u64,
    previous_stage_match: bool,
}

impl DeterministicTriggerEvaluator {
    fn observe(
        &mut self,
        trigger: &DeterministicTrigger,
        config: &DeterministicFakeConfig,
        sample: u64,
    ) -> bool {
        let Some(stage) = trigger.stages.get(self.stage) else {
            return true;
        };
        let mut matches = stage.predicates.iter().map(|predicate| {
            let previous = sample
                .checked_sub(1)
                .map(|previous| config.level_at(previous, predicate.channel));
            predicate
                .condition
                .matches(previous, config.level_at(sample, predicate.channel))
        });
        let first = matches
            .next()
            .expect("validated trigger stage is non-empty");
        let matched = match stage.logic {
            DeterministicTriggerLogic::And => matches.all(|matched| matched) && first,
            DeterministicTriggerLogic::Or => matches.any(|matched| matched) || first,
            DeterministicTriggerLogic::Xor => {
                matches.fold(first, |parity, matched| parity ^ matched)
            }
            DeterministicTriggerLogic::Nand => !(matches.all(|matched| matched) && first),
            DeterministicTriggerLogic::Nor => !(matches.any(|matched| matched) || first),
        } ^ stage.inverted;
        let qualified = match stage.count {
            None => matched,
            Some(count) => {
                match count.mode {
                    DeterministicTriggerCountMode::Occurrences => {
                        if matched && !self.previous_stage_match {
                            self.matched_count = self.matched_count.saturating_add(1);
                        }
                    }
                    DeterministicTriggerCountMode::Consecutive => {
                        self.matched_count = if matched {
                            self.matched_count.saturating_add(1)
                        } else {
                            0
                        };
                    }
                }
                self.matched_count >= count.value
            }
        };
        self.previous_stage_match = matched;
        if !qualified {
            return false;
        }
        if self.stage + 1 == trigger.stages.len() {
            return true;
        }
        self.stage += 1;
        self.matched_count = 0;
        self.previous_stage_match = false;
        false
    }
}

#[derive(Clone, Debug)]
pub struct DeterministicFakeConfig {
    channels: Arc<[CaptureChannelId]>,
    chunk_sample_counts: Arc<[u64]>,
    trigger: Option<Arc<DeterministicTrigger>>,
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
        let config = Self {
            trigger: None,
            channels,
            chunk_sample_counts,
            seed,
        };
        config.maximum_chunk_bytes()?;
        Ok(config)
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

    /// Configures a portable simple trigger. `None` disables the corresponding physical input.
    pub fn with_simple_trigger(
        mut self,
        conditions: impl Into<Arc<[Option<SimpleTriggerCondition>]>>,
    ) -> AcquisitionResult<Self> {
        let conditions = conditions.into();
        if conditions.len() != self.channels.len() {
            return Err(AcquisitionError::InvalidRequest(format!(
                "fake trigger has {} channels, expected {}",
                conditions.len(),
                self.channels.len()
            )));
        }
        let predicates = conditions
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(channel, condition)| {
                condition
                    .filter(|condition| *condition != SimpleTriggerCondition::Ignore)
                    .map(|condition| DeterministicTriggerPredicate { channel, condition })
            })
            .collect::<Vec<_>>();
        self.trigger = (!predicates.is_empty()).then(|| {
            Arc::new(DeterministicTrigger {
                stages: vec![DeterministicTriggerStage {
                    predicates,
                    logic: DeterministicTriggerLogic::And,
                    inverted: false,
                    count: None,
                }],
            })
        });
        Ok(self)
    }

    pub fn with_trigger(
        mut self,
        trigger: Option<DeterministicTrigger>,
    ) -> AcquisitionResult<Self> {
        if let Some(trigger) = &trigger {
            if trigger.stages.is_empty() {
                return Err(AcquisitionError::InvalidRequest(
                    "fake trigger requires at least one stage".into(),
                ));
            }
            for (stage_index, stage) in trigger.stages.iter().enumerate() {
                if stage.predicates.is_empty() {
                    return Err(AcquisitionError::InvalidRequest(format!(
                        "fake trigger stage {stage_index} requires at least one predicate"
                    )));
                }
                if let Some(predicate) = stage
                    .predicates
                    .iter()
                    .find(|predicate| predicate.channel >= self.channels.len())
                {
                    return Err(AcquisitionError::InvalidRequest(format!(
                        "fake trigger channel {} is outside 0..{}",
                        predicate.channel,
                        self.channels.len()
                    )));
                }
                if stage.count.is_some_and(|count| count.value == 0) {
                    return Err(AcquisitionError::InvalidRequest(format!(
                        "fake trigger stage {stage_index} count must be non-zero"
                    )));
                }
            }
        }
        self.trigger = trigger.map(Arc::new);
        Ok(self)
    }

    pub fn trigger(&self) -> Option<&DeterministicTrigger> {
        self.trigger.as_deref()
    }

    pub fn without_trigger(mut self) -> Self {
        self.trigger = None;
        self
    }

    pub fn has_trigger(&self) -> bool {
        self.trigger.is_some()
    }

    pub fn first_trigger_sample(&self) -> Option<u64> {
        let mut evaluator = DeterministicTriggerEvaluator::default();
        self.first_trigger_sample_in(0, self.total_samples(), &mut evaluator)
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

    fn first_trigger_sample_in(
        &self,
        start_sample: u64,
        sample_count: u64,
        evaluator: &mut DeterministicTriggerEvaluator,
    ) -> Option<u64> {
        let trigger = self.trigger.as_deref()?;
        let end_sample = start_sample.checked_add(sample_count)?;
        (start_sample..end_sample).find(|sample| evaluator.observe(trigger, self, *sample))
    }

    fn maximum_chunk_bytes(&self) -> AcquisitionResult<usize> {
        let samples = self.chunk_sample_counts.iter().copied().max().unwrap_or(0) as u128;
        let payload_bits = samples
            .checked_mul(self.channels.len() as u128)
            .and_then(|bits| bits.checked_add(7))
            .ok_or_else(|| AcquisitionError::Internal("fake payload size overflow".into()))?;
        usize::try_from(payload_bits.div_ceil(8))
            .map_err(|_| AcquisitionError::Internal("fake payload is too large".into()))
    }

    fn build_chunk(
        &self,
        buffer_pool: &CaptureBufferPool,
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
        let mut bytes = buffer_pool.acquire();
        bytes.resize(byte_len, 0);
        for relative_sample in 0..sample_count {
            for channel in 0..self.channels.len() {
                if self.level_at(start_sample + relative_sample, channel) {
                    let relative_bit = relative_sample as usize * self.channels.len() + channel;
                    let absolute_bit = usize::from(bit_offset) + relative_bit;
                    bytes.as_mut_slice()[absolute_bit / 8] |= 1 << (absolute_bit % 8);
                }
            }
        }
        CaptureChunk::packed_lsb_first(
            session_id,
            sequence,
            start_sample,
            sample_count,
            Arc::clone(&self.channels),
            bytes.freeze(),
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
    abort_requested: bool,
    force_trigger_requested: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FakeWake {
    Chunk,
    Stop,
    ForceTrigger,
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
                abort_requested: false,
                force_trigger_requested: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn wait_for_chunk(&self) -> FakeWake {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        while state.manual
            && state.permits == 0
            && !state.stop_requested
            && !state.abort_requested
            && !state.force_trigger_requested
        {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        if state.stop_requested || state.abort_requested {
            return FakeWake::Stop;
        }
        if state.force_trigger_requested {
            state.force_trigger_requested = false;
            return FakeWake::ForceTrigger;
        }
        if state.manual {
            state.permits -= 1;
        }
        FakeWake::Chunk
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

    fn request_abort(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.abort_requested = true;
        self.changed.notify_all();
    }

    fn request_force_trigger(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.force_trigger_requested = true;
        self.changed.notify_all();
    }

    fn was_aborted(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .abort_requested
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
    buffer_pool: CaptureBufferPool,
}

impl DeterministicFakeProvider {
    pub fn new(config: DeterministicFakeConfig) -> Self {
        let initial_capacity = config
            .maximum_chunk_bytes()
            .expect("validated fake configuration has a bounded chunk size");
        Self {
            config,
            control: Arc::new(FakeControl::new(false)),
            buffer_pool: CaptureBufferPool::new(2, initial_capacity)
                .expect("fake provider uses a non-zero pool size"),
        }
    }

    pub fn manually_paced(config: DeterministicFakeConfig) -> (Self, DeterministicFakeController) {
        let control = Arc::new(FakeControl::new(true));
        let initial_capacity = config
            .maximum_chunk_bytes()
            .expect("validated fake configuration has a bounded chunk size");
        (
            Self {
                config,
                control: Arc::clone(&control),
                buffer_pool: CaptureBufferPool::new(2, initial_capacity)
                    .expect("fake provider uses a non-zero pool size"),
            },
            DeterministicFakeController { control },
        )
    }

    pub fn buffer_pool(&self) -> CaptureBufferPool {
        self.buffer_pool.clone()
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
            buffer_pool: self.buffer_pool,
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
    buffer_pool: CaptureBufferPool,
    handle: Option<JoinHandle<AcquisitionResult<AcquisitionOutcome>>>,
    started: bool,
}

impl PreparedFakeAcquisition {
    fn run(
        mut context: AcquisitionContext,
        config: DeterministicFakeConfig,
        control: Arc<FakeControl>,
        buffer_pool: CaptureBufferPool,
    ) -> AcquisitionResult<AcquisitionOutcome> {
        let result = Self::run_inner(&mut context, &config, &control, &buffer_pool);
        if let Err(error) = &result {
            context.publish_failure(error);
        }
        result
    }

    fn run_inner(
        context: &mut AcquisitionContext,
        config: &DeterministicFakeConfig,
        control: &FakeControl,
        buffer_pool: &CaptureBufferPool,
    ) -> AcquisitionResult<AcquisitionOutcome> {
        let mut triggered = !config.has_trigger();
        if triggered {
            context.publish_status(
                CaptureSessionState::Recording,
                CaptureAcquisitionPhase::ReceivingLiveData,
            )?;
        } else {
            context.publish_status(
                CaptureSessionState::Armed,
                CaptureAcquisitionPhase::WaitingForTrigger,
            )?;
        }
        let mut captured_samples = 0_u64;
        let mut transferred_bytes = 0_u64;
        let mut chunk_count = 0_u64;
        let mut stopped = false;
        let mut trigger_evaluator = DeterministicTriggerEvaluator::default();
        for (sequence, sample_count) in config.chunk_sample_counts.iter().copied().enumerate() {
            loop {
                match control.wait_for_chunk() {
                    FakeWake::Chunk => break,
                    FakeWake::Stop => {
                        stopped = true;
                        break;
                    }
                    FakeWake::ForceTrigger if !triggered => {
                        triggered = true;
                        context.publish_triggered(captured_samples)?;
                        context.publish_status(
                            CaptureSessionState::Triggered,
                            CaptureAcquisitionPhase::ReceivingLiveData,
                        )?;
                        context.publish_status(
                            CaptureSessionState::Recording,
                            CaptureAcquisitionPhase::ReceivingLiveData,
                        )?;
                    }
                    FakeWake::ForceTrigger => {}
                }
            }
            if stopped {
                break;
            }
            let chunk = config.build_chunk(
                buffer_pool,
                context.session_id(),
                sequence as u64,
                captured_samples,
                sample_count,
            )?;
            let trigger_sample = (!triggered)
                .then(|| {
                    config.first_trigger_sample_in(
                        captured_samples,
                        sample_count,
                        &mut trigger_evaluator,
                    )
                })
                .flatten();
            transferred_bytes = transferred_bytes
                .checked_add(chunk.encoded_byte_len() as u64)
                .ok_or_else(|| AcquisitionError::Internal("byte count overflow".into()))?;
            context.append(chunk)?;
            if let Some(trigger_sample) = trigger_sample {
                triggered = true;
                context.publish_triggered(trigger_sample)?;
                context.publish_status(
                    CaptureSessionState::Triggered,
                    CaptureAcquisitionPhase::ReceivingLiveData,
                )?;
                context.publish_status(
                    CaptureSessionState::Recording,
                    CaptureAcquisitionPhase::ReceivingLiveData,
                )?;
            }
            captured_samples += sample_count;
            chunk_count += 1;
            context.publish_progress(CaptureProgress {
                captured_samples: Some(captured_samples),
                transferred_bytes: Some(transferred_bytes),
            })?;
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
            chunk_count,
            stopped,
            completion: if control.was_aborted() {
                CaptureCompletion::Aborted
            } else if stopped && config.has_trigger() && !triggered {
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

impl PreparedAcquisition for PreparedFakeAcquisition {
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
        let config = self.config.clone();
        let control = Arc::clone(&self.control);
        let buffer_pool = self.buffer_pool.clone();
        self.handle = Some(
            std::thread::Builder::new()
                .name("deterministic-live-capture".into())
                .spawn(move || Self::run(context, config, control, buffer_pool))
                .map_err(|error| AcquisitionError::WorkerStart(error.to_string()))?,
        );
        self.started = true;
        Ok(())
    }

    fn request_stop(&self) -> AcquisitionResult<()> {
        self.control.request_stop();
        Ok(())
    }

    fn request_abort(&self) -> AcquisitionResult<()> {
        self.control.request_abort();
        Ok(())
    }

    fn request_force_trigger(&self) -> AcquisitionResult<()> {
        self.control.request_force_trigger();
        Ok(())
    }

    fn is_finished(&self) -> bool {
        self.handle.as_ref().is_some_and(JoinHandle::is_finished)
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

    use tempfile::tempdir;

    use signal_processing::{
        AcquisitionContext, AcquisitionError, CaptureChannelId, CaptureCursorItem, CaptureEvent,
        CaptureQueueLimits, CaptureQueueReceiveError, CaptureSessionId, CaptureSessionState,
        CaptureStoreCursor, CaptureStoreDescriptor, NativeCaptureStore, NativeCaptureStoreConfig,
        NativeFinalizedCapture, bounded_capture_event_queue, bounded_capture_queue,
    };

    use super::{
        DeterministicFakeConfig, DeterministicFakeProvider, DeterministicTrigger,
        DeterministicTriggerCount, DeterministicTriggerCountMode, DeterministicTriggerLogic,
        DeterministicTriggerPredicate, DeterministicTriggerStage,
    };

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

    fn predicate(
        channel: usize,
        condition: signal_processing::SimpleTriggerCondition,
    ) -> DeterministicTriggerPredicate {
        DeterministicTriggerPredicate { channel, condition }
    }

    fn stage(
        logic: DeterministicTriggerLogic,
        predicates: Vec<DeterministicTriggerPredicate>,
    ) -> DeterministicTriggerStage {
        DeterministicTriggerStage {
            predicates,
            logic,
            inverted: false,
            count: None,
        }
    }

    #[test]
    fn lifecycle_reconstructs_exact_samples_with_bounded_queues() {
        let config = config();
        let session_id = CaptureSessionId::new(0x1234);
        let limits = CaptureQueueLimits::new(2, 16).unwrap();
        let (writer, chunks) = bounded_capture_queue(limits);
        let (events, event_reader) = bounded_capture_event_queue(32).unwrap();
        let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
        let provider = DeterministicFakeProvider::new(config.clone());
        let buffer_pool = provider.buffer_pool();
        let mut acquisition = provider.prepare(context).unwrap();

        assert_eq!(acquisition.session_id(), session_id);
        acquisition.start().unwrap();
        assert_eq!(acquisition.start(), Err(AcquisitionError::AlreadyStarted));
        let mut received = 0_usize;
        loop {
            match chunks.recv_timeout(TIMEOUT) {
                Ok(chunk) => {
                    assert_eq!(chunk.sequence(), received as u64);
                    for sample in 0..chunk.sample_count() {
                        for channel in 0..config.channels().len() {
                            assert_eq!(
                                chunk.packed_level(sample, channel),
                                Some(config.level_at(chunk.start_sample() + sample, channel))
                            );
                        }
                    }
                    received += 1;
                }
                Err(CaptureQueueReceiveError::Closed) => break,
                Err(error) => panic!("unexpected chunk receive error: {error}"),
            }
        }
        acquisition.request_stop().unwrap();
        acquisition.request_stop().unwrap();
        let outcome = acquisition.join().unwrap();

        assert_eq!(outcome.captured_samples, config.total_samples());
        assert_eq!(
            outcome.chunk_count as usize,
            config.chunk_sample_counts().len()
        );
        assert!(!outcome.stopped);
        assert!(chunks.max_observed_queued_chunks() <= chunks.capacity());
        assert_eq!(received, config.chunk_sample_counts().len());
        let pool_metrics = buffer_pool.metrics();
        assert!(pool_metrics.allocated <= pool_metrics.max_buffers);
        assert_eq!(pool_metrics.in_use, 0);

        let mut states = Vec::new();
        loop {
            match event_reader.recv_timeout(TIMEOUT) {
                Ok(CaptureEvent::Status(status)) => states.push(status.state),
                Ok(CaptureEvent::Progress { .. } | CaptureEvent::Health { .. }) => {}
                Ok(CaptureEvent::Plan { .. }) => {}
                Ok(CaptureEvent::Triggered { sample, .. }) => {
                    panic!("unexpected trigger at sample {sample}")
                }
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

    #[test]
    fn provider_round_trips_through_the_finalized_authoritative_store() {
        let config = config();
        let session_id = CaptureSessionId::new(0x9abc);
        let temporary = tempdir().unwrap();
        let descriptor =
            CaptureStoreDescriptor::new(session_id, config.channels().to_vec()).unwrap();
        let store_config = NativeCaptureStoreConfig::new(temporary.path(), descriptor)
            .with_commit_batch_chunks(2)
            .unwrap();
        let (store, writer) = NativeCaptureStore::create(store_config).unwrap();
        let (events, _event_reader) = bounded_capture_event_queue(32).unwrap();
        let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
        let provider = DeterministicFakeProvider::new(config.clone());
        let buffer_pool = provider.buffer_pool();
        let mut acquisition = provider.prepare(context).unwrap();

        acquisition.start().unwrap();
        let outcome = acquisition.join().unwrap();
        assert_eq!(outcome.captured_samples, config.total_samples());
        let pool_metrics = buffer_pool.metrics();
        assert_eq!(pool_metrics.allocated, 1);
        assert_eq!(pool_metrics.in_use, 0);
        assert_eq!(pool_metrics.available, 1);
        assert!(pool_metrics.max_in_use <= pool_metrics.max_buffers);
        assert!(!store.snapshot().writer_open);
        let finalized = store.finalize().unwrap();
        let reopened = NativeFinalizedCapture::open(finalized.directory()).unwrap();
        let mut cursor = reopened.open_cursor().unwrap();
        let mut reconstructed_samples = 0_u64;
        loop {
            match cursor.next().unwrap() {
                CaptureCursorItem::Chunk(chunk) => {
                    assert_eq!(chunk.start_sample(), reconstructed_samples);
                    for sample in 0..chunk.sample_count() {
                        for channel in 0..config.channels().len() {
                            assert_eq!(
                                chunk.packed_level(sample, channel),
                                Some(config.level_at(reconstructed_samples + sample, channel))
                            );
                        }
                    }
                    reconstructed_samples = chunk.end_sample();
                }
                CaptureCursorItem::End => break,
                CaptureCursorItem::Pending => panic!("finalized cursor cannot be pending"),
            }
        }
        assert_eq!(reconstructed_samples, config.total_samples());
    }

    #[test]
    fn portable_conditions_publish_the_exact_deterministic_trigger_sample() {
        use signal_processing::SimpleTriggerCondition::{Either, Falling, High, Low, Rising};

        for (condition, expected) in [(Low, 2), (High, 0), (Rising, 3), (Falling, 2), (Either, 2)] {
            let config = config()
                .with_simple_trigger(vec![Some(condition), None, None])
                .unwrap();
            assert_eq!(config.first_trigger_sample(), Some(expected));
            let session_id = CaptureSessionId::new(0x7000 + condition as u128);
            let temporary = tempdir().unwrap();
            let descriptor =
                CaptureStoreDescriptor::new(session_id, config.channels().to_vec()).unwrap();
            let (store, writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
                temporary.path(),
                descriptor,
            ))
            .unwrap();
            let (events, event_reader) = bounded_capture_event_queue(64).unwrap();
            let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
            let mut acquisition = DeterministicFakeProvider::new(config)
                .prepare(context)
                .unwrap();

            acquisition.start().unwrap();
            acquisition.join().unwrap();
            store.finalize().unwrap();

            let mut states = Vec::new();
            let mut actual = None;
            loop {
                match event_reader.recv_timeout(TIMEOUT) {
                    Ok(CaptureEvent::Status(status)) => states.push(status.state),
                    Ok(CaptureEvent::Triggered { sample, .. }) => actual = Some(sample),
                    Ok(CaptureEvent::Progress { .. } | CaptureEvent::Health { .. }) => {}
                    Ok(CaptureEvent::Plan { .. }) => {}
                    Ok(CaptureEvent::Failed(failure)) => {
                        panic!("unexpected failure: {failure:?}")
                    }
                    Err(CaptureQueueReceiveError::Closed) => break,
                    Err(error) => panic!("unexpected event receive error: {error}"),
                }
            }
            assert_eq!(actual, Some(expected), "condition {condition:?}");
            assert!(states.windows(2).any(|states| {
                states == [CaptureSessionState::Armed, CaptureSessionState::Triggered]
            }));
            assert!(states.windows(2).any(|states| {
                states
                    == [
                        CaptureSessionState::Triggered,
                        CaptureSessionState::Recording,
                    ]
            }));
        }
    }

    #[test]
    fn staged_trigger_executes_every_logic_operator_and_inversion() {
        use signal_processing::SimpleTriggerCondition::High;

        let predicates = vec![predicate(0, High), predicate(2, High)];
        for (logic, expected) in [
            (DeterministicTriggerLogic::And, 1),
            (DeterministicTriggerLogic::Or, 0),
            (DeterministicTriggerLogic::Xor, 0),
            (DeterministicTriggerLogic::Nand, 0),
            (DeterministicTriggerLogic::Nor, 11),
        ] {
            let configured = config()
                .with_trigger(Some(DeterministicTrigger {
                    stages: vec![stage(logic, predicates.clone())],
                }))
                .unwrap();
            assert_eq!(
                configured.first_trigger_sample(),
                Some(expected),
                "{logic:?}"
            );
        }

        let mut inverted = stage(DeterministicTriggerLogic::Or, predicates);
        inverted.inverted = true;
        let configured = config()
            .with_trigger(Some(DeterministicTrigger {
                stages: vec![inverted],
            }))
            .unwrap();
        assert_eq!(configured.first_trigger_sample(), Some(11));
    }

    #[test]
    fn staged_trigger_counts_and_stage_progress_cross_chunk_boundaries() {
        use signal_processing::SimpleTriggerCondition::{Falling, High, Rising};

        let mut occurrences = stage(DeterministicTriggerLogic::And, vec![predicate(0, High)]);
        occurrences.count = Some(DeterministicTriggerCount {
            mode: DeterministicTriggerCountMode::Occurrences,
            value: 2,
        });
        let configured = config()
            .with_trigger(Some(DeterministicTrigger {
                stages: vec![occurrences],
            }))
            .unwrap();
        assert_eq!(configured.chunk_sample_counts()[0], 3);
        assert_eq!(configured.first_trigger_sample(), Some(3));

        let mut consecutive = stage(DeterministicTriggerLogic::And, vec![predicate(0, High)]);
        consecutive.count = Some(DeterministicTriggerCount {
            mode: DeterministicTriggerCountMode::Consecutive,
            value: 2,
        });
        let configured = config()
            .with_trigger(Some(DeterministicTrigger {
                stages: vec![consecutive],
            }))
            .unwrap();
        assert_eq!(configured.first_trigger_sample(), Some(1));

        let configured = config()
            .with_trigger(Some(DeterministicTrigger {
                stages: vec![
                    stage(DeterministicTriggerLogic::And, vec![predicate(0, Falling)]),
                    stage(DeterministicTriggerLogic::And, vec![predicate(0, Rising)]),
                ],
            }))
            .unwrap();
        assert_eq!(configured.first_trigger_sample(), Some(3));
    }

    #[test]
    fn disabled_and_ignore_conditions_do_not_arm_the_fake_provider() {
        use signal_processing::SimpleTriggerCondition::{High, Ignore};

        for conditions in [
            vec![None, None, None],
            vec![Some(Ignore), None, None],
            vec![None, Some(Ignore), Some(Ignore)],
        ] {
            let config = config().with_simple_trigger(conditions).unwrap();
            assert!(!config.has_trigger());
            assert_eq!(config.first_trigger_sample(), None);
        }

        let enabled = config()
            .with_simple_trigger(vec![None, Some(High), None])
            .unwrap();
        assert!(enabled.has_trigger());
        assert!(enabled.first_trigger_sample().is_some());
    }
}
