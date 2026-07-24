use std::sync::Arc;

use thiserror::Error;

use super::conditions::{PinCondition, WaitCondition, WaitRequest, WaitTerm};

const DISCONNECTED_PIN_VALUE: u8 = 0xff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InitialPin {
    Low,
    High,
    SameAsFirstSample,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogicChunk {
    start_sample: u64,
    sample_count: usize,
    channels: Vec<Option<Arc<[u8]>>>,
}

impl LogicChunk {
    pub(crate) fn new(
        start_sample: u64,
        sample_count: usize,
        channels: Vec<Option<Arc<[u8]>>>,
    ) -> Self {
        Self {
            start_sample,
            sample_count,
            channels,
        }
    }

    pub(crate) fn sample_count(&self) -> usize {
        self.sample_count
    }

    fn end_sample(&self) -> Option<u64> {
        self.start_sample.checked_add(self.sample_count as u64)
    }

    fn pin(&self, channel: usize, sample: usize) -> Option<bool> {
        self.channels[channel].as_ref().map(|data| {
            let byte = data[sample / 8];
            (byte >> (sample % 8)) & 1 != 0
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WaitMatch {
    pub(crate) sample: u64,
    pub(crate) pins: Vec<u8>,
    pub(crate) matched: Option<Vec<bool>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SchedulerStatus {
    Waiting,
    Matched(WaitMatch),
    EndOfStream,
    Cancelled,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum SchedulerError {
    #[error("a Sigrok decoder requires at least one channel")]
    NoChannels,
    #[error("wait() is already active")]
    AlreadyWaiting,
    #[error("sample input arrived without an active wait()")]
    NoActiveWait,
    #[error("sample input arrived after end of stream")]
    InputAfterEndOfStream,
    #[error("sample input remains at the current wait() match")]
    PendingInput,
    #[error("empty sample chunks are not valid")]
    EmptyChunk,
    #[error("sample range overflows u64")]
    SampleRangeOverflow,
    #[error("expected chunk at sample {expected}, got {actual}")]
    UnexpectedChunkStart { expected: u64, actual: u64 },
    #[error("expected {expected} channels, got {actual}")]
    ChannelCount { expected: usize, actual: usize },
    #[error("connected channel {channel} has no sample data")]
    MissingChannelData { channel: usize },
    #[error("disconnected channel {channel} unexpectedly has sample data")]
    UnexpectedChannelData { channel: usize },
    #[error("channel {channel} needs {required} bytes, got {actual}")]
    ShortChannelData {
        channel: usize,
        required: usize,
        actual: usize,
    },
}

#[derive(Clone, Debug)]
struct PendingChunk {
    chunk: LogicChunk,
    next_sample: usize,
}

#[derive(Clone, Debug)]
struct ActiveWait {
    conditions: Vec<ConditionState>,
    automatic: bool,
}

impl ActiveWait {
    fn from_request(request: WaitRequest, first_wait: bool) -> Self {
        match request {
            WaitRequest::Next => Self::next(first_wait),
            WaitRequest::Conditions(conditions) if conditions.is_empty() => Self::next(first_wait),
            WaitRequest::Conditions(conditions) => {
                let automatic = conditions
                    .iter()
                    .all(|condition| condition.terms().is_empty());
                Self {
                    conditions: conditions.into_iter().map(ConditionState::new).collect(),
                    automatic,
                }
            }
        }
    }

    fn next(first_wait: bool) -> Self {
        Self {
            conditions: vec![ConditionState::new(WaitCondition::new([WaitTerm::Skip(
                u64::from(!first_wait),
            )]))],
            automatic: false,
        }
    }
}

#[derive(Clone, Debug)]
struct ConditionState {
    terms: Vec<TermState>,
}

impl ConditionState {
    fn new(condition: WaitCondition) -> Self {
        Self {
            terms: condition
                .terms()
                .iter()
                .cloned()
                .map(TermState::new)
                .collect(),
        }
    }

    fn matches(&mut self, current: &[Option<bool>], previous: &[Option<bool>]) -> bool {
        if self.terms.is_empty() {
            return false;
        }
        for term in &mut self.terms {
            if !term.matches(current, previous) {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Debug)]
enum TermState {
    Pin {
        channel: usize,
        condition: PinCondition,
    },
    Skip {
        target: u64,
        skipped: u64,
    },
    Never,
}

impl TermState {
    fn new(term: WaitTerm) -> Self {
        match term {
            WaitTerm::Pin { channel, condition } => Self::Pin { channel, condition },
            WaitTerm::Skip(target) => Self::Skip { target, skipped: 0 },
            WaitTerm::Never => Self::Never,
        }
    }

    fn matches(&mut self, current: &[Option<bool>], previous: &[Option<bool>]) -> bool {
        match self {
            Self::Pin { channel, condition } => {
                let Some(Some(current)) = current.get(*channel) else {
                    return false;
                };
                let Some(Some(previous)) = previous.get(*channel) else {
                    return false;
                };
                match condition {
                    PinCondition::High => *current,
                    PinCondition::Low => !*current,
                    PinCondition::Rising => !*previous && *current,
                    PinCondition::Falling => *previous && !*current,
                    PinCondition::EitherEdge => previous != current,
                    PinCondition::NoEdge => previous == current,
                }
            }
            Self::Skip { target, skipped } => {
                if skipped == target {
                    true
                } else {
                    *skipped = skipped.saturating_add(1);
                    false
                }
            }
            Self::Never => false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct WaitScheduler {
    channel_initial: Vec<Option<InitialPin>>,
    previous_pins: Vec<Option<bool>>,
    initialized_first_sample: bool,
    next_input_sample: u64,
    pending: Option<PendingChunk>,
    active_wait: Option<ActiveWait>,
    waits_started: u64,
    end_of_stream: bool,
    cancelled: bool,
}

impl WaitScheduler {
    pub(crate) fn new(channel_initial: Vec<Option<InitialPin>>) -> Result<Self, SchedulerError> {
        if channel_initial.is_empty() {
            return Err(SchedulerError::NoChannels);
        }
        let previous_pins = channel_initial
            .iter()
            .map(|initial| match initial {
                Some(InitialPin::Low) => Some(false),
                Some(InitialPin::High) => Some(true),
                Some(InitialPin::SameAsFirstSample) | None => None,
            })
            .collect();
        Ok(Self {
            channel_initial,
            previous_pins,
            initialized_first_sample: false,
            next_input_sample: 0,
            pending: None,
            active_wait: None,
            waits_started: 0,
            end_of_stream: false,
            cancelled: false,
        })
    }

    pub(crate) fn begin_wait(
        &mut self,
        request: WaitRequest,
    ) -> Result<SchedulerStatus, SchedulerError> {
        if self.cancelled {
            return Ok(SchedulerStatus::Cancelled);
        }
        if self.active_wait.is_some() {
            return Err(SchedulerError::AlreadyWaiting);
        }
        let first_wait = self.waits_started == 0;
        self.waits_started = self.waits_started.saturating_add(1);
        self.active_wait = Some(ActiveWait::from_request(request, first_wait));
        self.scan()
    }

    pub(crate) fn push_chunk(
        &mut self,
        chunk: LogicChunk,
    ) -> Result<SchedulerStatus, SchedulerError> {
        if self.cancelled {
            return Ok(SchedulerStatus::Cancelled);
        }
        if self.end_of_stream {
            return Err(SchedulerError::InputAfterEndOfStream);
        }
        if self.active_wait.is_none() {
            return Err(SchedulerError::NoActiveWait);
        }
        if self.pending.is_some() {
            return Err(SchedulerError::PendingInput);
        }
        self.validate_chunk(&chunk)?;
        self.pending = Some(PendingChunk {
            chunk,
            next_sample: 0,
        });
        self.scan()
    }

    pub(crate) fn finish(&mut self) -> Result<SchedulerStatus, SchedulerError> {
        self.end_of_stream = true;
        self.scan()
    }

    pub(crate) fn cancel(&mut self) -> SchedulerStatus {
        self.cancelled = true;
        self.active_wait = None;
        SchedulerStatus::Cancelled
    }

    fn validate_chunk(&self, chunk: &LogicChunk) -> Result<(), SchedulerError> {
        if chunk.sample_count == 0 {
            return Err(SchedulerError::EmptyChunk);
        }
        chunk
            .end_sample()
            .ok_or(SchedulerError::SampleRangeOverflow)?;
        if chunk.start_sample != self.next_input_sample {
            return Err(SchedulerError::UnexpectedChunkStart {
                expected: self.next_input_sample,
                actual: chunk.start_sample,
            });
        }
        if chunk.channels.len() != self.channel_initial.len() {
            return Err(SchedulerError::ChannelCount {
                expected: self.channel_initial.len(),
                actual: chunk.channels.len(),
            });
        }
        let required = chunk.sample_count.div_ceil(8);
        for (channel, (initial, data)) in
            self.channel_initial.iter().zip(&chunk.channels).enumerate()
        {
            match (initial, data) {
                (Some(_), None) => return Err(SchedulerError::MissingChannelData { channel }),
                (None, Some(_)) => return Err(SchedulerError::UnexpectedChannelData { channel }),
                (Some(_), Some(data)) if data.len() < required => {
                    return Err(SchedulerError::ShortChannelData {
                        channel,
                        required,
                        actual: data.len(),
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn scan(&mut self) -> Result<SchedulerStatus, SchedulerError> {
        if self.cancelled {
            return Ok(SchedulerStatus::Cancelled);
        }
        if self.active_wait.is_none() {
            return if self.end_of_stream && self.pending.is_none() {
                Ok(SchedulerStatus::EndOfStream)
            } else {
                Ok(SchedulerStatus::Waiting)
            };
        }

        while self.pending.is_some() {
            let (absolute_sample, current_pins) = {
                let pending = self.pending.as_ref().expect("checked above");
                let absolute_sample = pending
                    .chunk
                    .start_sample
                    .checked_add(pending.next_sample as u64)
                    .ok_or(SchedulerError::SampleRangeOverflow)?;
                let current_pins = (0..self.channel_initial.len())
                    .map(|channel| pending.chunk.pin(channel, pending.next_sample))
                    .collect::<Vec<_>>();
                (absolute_sample, current_pins)
            };

            let automatic = self
                .active_wait
                .as_ref()
                .is_some_and(|active| active.automatic);
            if automatic {
                let pins = pin_values(&current_pins);
                self.active_wait = None;
                return Ok(SchedulerStatus::Matched(WaitMatch {
                    sample: absolute_sample,
                    pins,
                    matched: None,
                }));
            }

            self.initialize_previous_pins(&current_pins);
            let matched = self
                .active_wait
                .as_mut()
                .expect("active wait retained while scanning")
                .conditions
                .iter_mut()
                .map(|condition| condition.matches(&current_pins, &self.previous_pins))
                .collect::<Vec<_>>();
            self.previous_pins.clone_from(&current_pins);

            if matched.iter().any(|matched| *matched) {
                let pins = pin_values(&current_pins);
                self.active_wait = None;
                return Ok(SchedulerStatus::Matched(WaitMatch {
                    sample: absolute_sample,
                    pins,
                    matched: Some(matched),
                }));
            }

            let pending = self.pending.as_mut().expect("checked above");
            pending.next_sample += 1;
            self.next_input_sample = absolute_sample
                .checked_add(1)
                .ok_or(SchedulerError::SampleRangeOverflow)?;
            if pending.next_sample == pending.chunk.sample_count {
                self.pending = None;
            }
        }

        if self.end_of_stream {
            self.active_wait = None;
            Ok(SchedulerStatus::EndOfStream)
        } else {
            Ok(SchedulerStatus::Waiting)
        }
    }

    fn initialize_previous_pins(&mut self, current: &[Option<bool>]) {
        if self.initialized_first_sample {
            return;
        }
        for (channel, initial) in self.channel_initial.iter().enumerate() {
            if matches!(initial, Some(InitialPin::SameAsFirstSample)) {
                self.previous_pins[channel] = current[channel];
            }
        }
        self.initialized_first_sample = true;
    }
}

fn pin_values(pins: &[Option<bool>]) -> Vec<u8> {
    pins.iter()
        .map(|pin| pin.map_or(DISCONNECTED_PIN_VALUE, u8::from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pin_condition_uses_api_v3_semantics() {
        let samples = [false, false, true, true, false, false];
        let cases = [
            (PinCondition::Low, 0),
            (PinCondition::High, 2),
            (PinCondition::Rising, 2),
            (PinCondition::Falling, 4),
            (PinCondition::EitherEdge, 2),
            (PinCondition::NoEdge, 0),
        ];
        for (condition, expected) in cases {
            let mut scheduler = scheduler([Some(InitialPin::SameAsFirstSample)]);
            assert_eq!(
                scheduler.begin_wait(pin_wait(0, condition)).unwrap(),
                SchedulerStatus::Waiting
            );
            let status = scheduler.push_chunk(chunk(0, &[&samples])).unwrap();
            assert_eq!(matched_sample(status), expected, "{condition:?}");
        }
    }

    #[test]
    fn explicit_initial_pins_can_create_an_edge_at_sample_zero() {
        let mut rising = scheduler([Some(InitialPin::Low)]);
        rising
            .begin_wait(pin_wait(0, PinCondition::Rising))
            .unwrap();
        assert_eq!(
            matched_sample(rising.push_chunk(chunk(0, &[&[true]])).unwrap()),
            0
        );

        let mut falling = scheduler([Some(InitialPin::High)]);
        falling
            .begin_wait(pin_wait(0, PinCondition::Falling))
            .unwrap();
        assert_eq!(
            matched_sample(falling.push_chunk(chunk(0, &[&[false]])).unwrap()),
            0
        );

        let mut same = scheduler([Some(InitialPin::SameAsFirstSample)]);
        same.begin_wait(pin_wait(0, PinCondition::Rising)).unwrap();
        assert_eq!(
            same.push_chunk(chunk(0, &[&[true]])).unwrap(),
            SchedulerStatus::Waiting
        );
        assert_eq!(same.finish().unwrap(), SchedulerStatus::EndOfStream);
    }

    #[test]
    fn alternatives_are_all_evaluated_and_reported_in_order() {
        let mut scheduler = scheduler([
            Some(InitialPin::SameAsFirstSample),
            Some(InitialPin::SameAsFirstSample),
        ]);
        scheduler
            .begin_wait(WaitRequest::Conditions(vec![
                WaitCondition::new([WaitTerm::pin(0, PinCondition::Rising)]),
                WaitCondition::new([WaitTerm::pin(1, PinCondition::High)]),
                WaitCondition::new([WaitTerm::Never]),
            ]))
            .unwrap();
        let status = scheduler
            .push_chunk(chunk(0, &[&[false, false, true], &[false, false, true]]))
            .unwrap();
        let SchedulerStatus::Matched(result) = status else {
            panic!("expected a match");
        };
        assert_eq!(result.sample, 2);
        assert_eq!(result.pins, [1, 1]);
        assert_eq!(result.matched, Some(vec![true, true, false]));
    }

    #[test]
    fn terms_are_and_combined_and_preserve_skip_evaluation_order() {
        let request = WaitRequest::Conditions(vec![WaitCondition::new([
            WaitTerm::pin(0, PinCondition::High),
            WaitTerm::Skip(2),
        ])]);
        let samples = [false, false, true, true, true];
        let mut scheduler = scheduler([Some(InitialPin::SameAsFirstSample)]);
        scheduler.begin_wait(request).unwrap();
        let status = scheduler.push_chunk(chunk(0, &[&samples])).unwrap();
        assert_eq!(matched_sample(status), 4);
    }

    #[test]
    fn conditionless_wait_advances_after_its_first_match() {
        let mut scheduler = scheduler([Some(InitialPin::SameAsFirstSample)]);
        assert_eq!(
            scheduler.begin_wait(WaitRequest::Next).unwrap(),
            SchedulerStatus::Waiting
        );
        assert_eq!(
            matched_sample(
                scheduler
                    .push_chunk(chunk(0, &[&[false, true, false, true, false]]))
                    .unwrap()
            ),
            0
        );
        assert_eq!(
            matched_sample(scheduler.begin_wait(WaitRequest::Next).unwrap()),
            1
        );
        assert_eq!(
            matched_sample(
                scheduler
                    .begin_wait(WaitRequest::Conditions(vec![WaitCondition::new([
                        WaitTerm::Skip(3),
                    ])]))
                    .unwrap()
            ),
            4
        );
    }

    #[test]
    fn matches_are_invariant_across_every_chunk_boundary() {
        let samples = [false, false, false, true, true, false, true, true];
        for split in 1..samples.len() {
            let mut scheduler = scheduler([Some(InitialPin::SameAsFirstSample)]);
            scheduler
                .begin_wait(pin_wait(0, PinCondition::Rising))
                .unwrap();
            assert_eq!(
                scheduler
                    .push_chunk(chunk(0, &[&samples[..split]]))
                    .unwrap(),
                if split > 3 {
                    SchedulerStatus::Matched(WaitMatch {
                        sample: 3,
                        pins: vec![1],
                        matched: Some(vec![true]),
                    })
                } else {
                    SchedulerStatus::Waiting
                }
            );
            if split <= 3 {
                assert_eq!(
                    matched_sample(
                        scheduler
                            .push_chunk(chunk(split as u64, &[&samples[split..]]))
                            .unwrap()
                    ),
                    3,
                    "split at {split}"
                );
            }
        }
    }

    #[test]
    fn skip_state_continues_across_chunks() {
        let mut scheduler = scheduler([Some(InitialPin::SameAsFirstSample)]);
        scheduler
            .begin_wait(WaitRequest::Conditions(vec![WaitCondition::new([
                WaitTerm::Skip(5),
            ])]))
            .unwrap();
        assert_eq!(
            scheduler.push_chunk(chunk(0, &[&[false, false]])).unwrap(),
            SchedulerStatus::Waiting
        );
        assert_eq!(
            scheduler
                .push_chunk(chunk(2, &[&[false, false, false, false]]))
                .unwrap(),
            SchedulerStatus::Matched(WaitMatch {
                sample: 5,
                pins: vec![0],
                matched: Some(vec![true]),
            })
        );
    }

    #[test]
    fn disconnected_channels_return_ff_and_never_satisfy_pin_terms() {
        let mut scheduler = scheduler([Some(InitialPin::SameAsFirstSample), None]);
        scheduler.begin_wait(WaitRequest::Next).unwrap();
        assert_eq!(
            scheduler
                .push_chunk(LogicChunk::new(0, 1, vec![Some(packed(&[true])), None]))
                .unwrap(),
            SchedulerStatus::Matched(WaitMatch {
                sample: 0,
                pins: vec![1, DISCONNECTED_PIN_VALUE],
                matched: Some(vec![true]),
            })
        );

        scheduler
            .begin_wait(pin_wait(1, PinCondition::High))
            .unwrap();
        assert_eq!(scheduler.finish().unwrap(), SchedulerStatus::EndOfStream);
    }

    #[test]
    fn all_empty_alternatives_match_without_a_matched_tuple() {
        let mut scheduler = scheduler([Some(InitialPin::SameAsFirstSample)]);
        scheduler
            .begin_wait(WaitRequest::Conditions(vec![WaitCondition::default()]))
            .unwrap();
        assert_eq!(
            scheduler.push_chunk(chunk(0, &[&[true]])).unwrap(),
            SchedulerStatus::Matched(WaitMatch {
                sample: 0,
                pins: vec![1],
                matched: None,
            })
        );
    }

    #[test]
    fn eof_and_cancellation_terminate_an_active_wait() {
        let mut eof = scheduler([Some(InitialPin::SameAsFirstSample)]);
        eof.begin_wait(pin_wait(0, PinCondition::Rising)).unwrap();
        assert_eq!(eof.finish().unwrap(), SchedulerStatus::EndOfStream);
        assert_eq!(
            eof.begin_wait(WaitRequest::Next).unwrap(),
            SchedulerStatus::EndOfStream
        );
        assert_eq!(
            eof.push_chunk(chunk(0, &[&[false]])).unwrap_err(),
            SchedulerError::InputAfterEndOfStream
        );

        let mut cancelled = scheduler([Some(InitialPin::SameAsFirstSample)]);
        cancelled
            .begin_wait(pin_wait(0, PinCondition::Rising))
            .unwrap();
        assert_eq!(cancelled.cancel(), SchedulerStatus::Cancelled);
        assert_eq!(
            cancelled.begin_wait(WaitRequest::Next).unwrap(),
            SchedulerStatus::Cancelled
        );
    }

    #[test]
    fn malformed_or_discontinuous_chunks_are_rejected() {
        let mut scheduler = scheduler([Some(InitialPin::SameAsFirstSample)]);
        scheduler.begin_wait(WaitRequest::Next).unwrap();
        assert_eq!(
            scheduler.push_chunk(chunk(1, &[&[false]])).unwrap_err(),
            SchedulerError::UnexpectedChunkStart {
                expected: 0,
                actual: 1,
            }
        );
        assert_eq!(
            scheduler
                .push_chunk(LogicChunk::new(0, 9, vec![Some(Arc::from([0_u8]))]))
                .unwrap_err(),
            SchedulerError::ShortChannelData {
                channel: 0,
                required: 2,
                actual: 1,
            }
        );
    }

    fn scheduler<const N: usize>(initial: [Option<InitialPin>; N]) -> WaitScheduler {
        WaitScheduler::new(initial.into()).unwrap()
    }

    fn pin_wait(channel: usize, condition: PinCondition) -> WaitRequest {
        WaitRequest::Conditions(vec![WaitCondition::new([WaitTerm::pin(
            channel, condition,
        )])])
    }

    fn chunk(start_sample: u64, channels: &[&[bool]]) -> LogicChunk {
        let sample_count = channels.first().map_or(0, |channel| channel.len());
        assert!(channels.iter().all(|channel| channel.len() == sample_count));
        LogicChunk::new(
            start_sample,
            sample_count,
            channels
                .iter()
                .map(|channel| Some(packed(channel)))
                .collect(),
        )
    }

    fn packed(samples: &[bool]) -> Arc<[u8]> {
        let mut bytes = vec![0_u8; samples.len().div_ceil(8)];
        for (sample, value) in samples.iter().enumerate() {
            bytes[sample / 8] |= u8::from(*value) << (sample % 8);
        }
        bytes.into()
    }

    fn matched_sample(status: SchedulerStatus) -> u64 {
        let SchedulerStatus::Matched(result) = status else {
            panic!("expected a match, got {status:?}");
        };
        result.sample
    }
}
