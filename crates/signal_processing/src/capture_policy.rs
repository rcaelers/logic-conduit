//! Generic capture-policy negotiation, capacity estimation, and retention safety.
//!
//! This module deliberately knows nothing about transports, devices, graph nodes, or UI widgets.
//! Concrete capture features advertise the subset they can implement and persist their requested
//! policy in their own state.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingStart {
    Immediate,
    Trigger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CaptureFraction(u16);

impl CaptureFraction {
    pub const DENOMINATOR: u16 = 10_000;

    pub fn new(parts: u16) -> Result<Self, CapturePolicyError> {
        if parts > Self::DENOMINATOR {
            return Err(CapturePolicyError::Invalid(
                "capture fraction must be within 0..=10000".into(),
            ));
        }
        Ok(Self(parts))
    }

    pub fn from_percent(percent: u8) -> Result<Self, CapturePolicyError> {
        Self::new(u16::from(percent) * 100)
    }

    pub const fn parts(self) -> u16 {
        self.0
    }

    pub const fn percent_floor(self) -> u8 {
        (self.0 / 100) as u8
    }

    pub fn samples_of(self, samples: u64) -> u64 {
        ((u128::from(samples) * u128::from(self.0)) / u128::from(Self::DENOMINATOR)) as u64
    }
}

impl Default for CaptureFraction {
    fn default() -> Self {
        Self(Self::DENOMINATOR / 2)
    }
}

impl From<CaptureFraction> for u16 {
    fn from(value: CaptureFraction) -> Self {
        value.0
    }
}

impl TryFrom<u16> for CaptureFraction {
    type Error = CapturePolicyError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for CaptureFraction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u16(self.0)
    }
}

impl<'de> Deserialize<'de> for CaptureFraction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(u16::deserialize(deserializer)?)
            .map_err(|error| serde::de::Error::custom(error.to_string()))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerPlacement {
    Fraction(CaptureFraction),
    SamplesBefore(u64),
    DurationBefore(Duration),
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPolicy {
    Everything,
    RecentDuration(Duration),
    RecentBytes(u64),
    DeviceManaged,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionPolicy {
    UntilStopped,
    DurationAfterOrigin(Duration),
    SamplesAfterOrigin(u64),
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerTimeoutAction {
    ContinueWaiting,
    Stop,
    ForceTrigger,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct TriggerTimeout {
    pub after: Duration,
    pub action: TriggerTimeoutAction,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CapturePolicy {
    pub start: RecordingStart,
    pub trigger_placement: Option<TriggerPlacement>,
    pub retention_before_origin: RetentionPolicy,
    pub retention_after_origin: RetentionPolicy,
    pub completion: CompletionPolicy,
    pub trigger_timeout: Option<TriggerTimeout>,
}

impl Default for CapturePolicy {
    fn default() -> Self {
        Self {
            start: RecordingStart::Immediate,
            trigger_placement: None,
            retention_before_origin: RetentionPolicy::Everything,
            retention_after_origin: RetentionPolicy::Everything,
            completion: CompletionPolicy::UntilStopped,
            trigger_timeout: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionPolicyKind {
    Everything,
    RecentDuration,
    RecentBytes,
    DeviceManaged,
}

impl From<RetentionPolicy> for RetentionPolicyKind {
    fn from(value: RetentionPolicy) -> Self {
        match value {
            RetentionPolicy::Everything => Self::Everything,
            RetentionPolicy::RecentDuration(_) => Self::RecentDuration,
            RetentionPolicy::RecentBytes(_) => Self::RecentBytes,
            RetentionPolicy::DeviceManaged => Self::DeviceManaged,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionPolicyKind {
    UntilStopped,
    DurationAfterOrigin,
    SamplesAfterOrigin,
}

impl From<CompletionPolicy> for CompletionPolicyKind {
    fn from(value: CompletionPolicy) -> Self {
        match value {
            CompletionPolicy::UntilStopped => Self::UntilStopped,
            CompletionPolicy::DurationAfterOrigin(_) => Self::DurationAfterOrigin,
            CompletionPolicy::SamplesAfterOrigin(_) => Self::SamplesAfterOrigin,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerPlacementCapability {
    Unsupported,
    Fixed(TriggerPlacement),
    SelectableFraction {
        minimum: CaptureFraction,
        maximum: CaptureFraction,
        step: CaptureFraction,
        sample_alignment: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturePolicyCapabilities {
    recording_starts: Arc<[RecordingStart]>,
    retention: Arc<[RetentionPolicyKind]>,
    completion: Arc<[CompletionPolicyKind]>,
    trigger_placement: TriggerPlacementCapability,
    trigger_timeout_actions: Arc<[TriggerTimeoutAction]>,
}

impl CapturePolicyCapabilities {
    pub fn new(
        recording_starts: impl Into<Arc<[RecordingStart]>>,
        retention: impl Into<Arc<[RetentionPolicyKind]>>,
        completion: impl Into<Arc<[CompletionPolicyKind]>>,
        trigger_placement: TriggerPlacementCapability,
        trigger_timeout_actions: impl Into<Arc<[TriggerTimeoutAction]>>,
    ) -> Result<Self, CapturePolicyError> {
        let recording_starts = recording_starts.into();
        let retention = retention.into();
        let completion = completion.into();
        let trigger_timeout_actions = trigger_timeout_actions.into();
        if recording_starts.is_empty() || retention.is_empty() || completion.is_empty() {
            return Err(CapturePolicyError::Invalid(
                "capture-policy capabilities require start, retention, and completion choices"
                    .into(),
            ));
        }
        if let TriggerPlacementCapability::SelectableFraction {
            minimum,
            maximum,
            step,
            sample_alignment,
        } = trigger_placement
            && (minimum > maximum || step.parts() == 0 || sample_alignment == 0)
        {
            return Err(CapturePolicyError::Invalid(
                "selectable trigger placement has an invalid range or alignment".into(),
            ));
        }
        Ok(Self {
            recording_starts,
            retention,
            completion,
            trigger_placement,
            trigger_timeout_actions,
        })
    }

    pub fn finite_default() -> Self {
        Self::new(
            Arc::from([RecordingStart::Immediate, RecordingStart::Trigger]),
            Arc::from([RetentionPolicyKind::Everything]),
            Arc::from([
                CompletionPolicyKind::DurationAfterOrigin,
                CompletionPolicyKind::SamplesAfterOrigin,
            ]),
            TriggerPlacementCapability::Fixed(TriggerPlacement::Fraction(
                CaptureFraction::default(),
            )),
            Arc::from([
                TriggerTimeoutAction::ContinueWaiting,
                TriggerTimeoutAction::Stop,
            ]),
        )
        .expect("default finite policy capabilities are valid")
    }

    pub fn continuous_default() -> Self {
        Self::new(
            Arc::from([RecordingStart::Immediate, RecordingStart::Trigger]),
            Arc::from([
                RetentionPolicyKind::Everything,
                RetentionPolicyKind::RecentDuration,
                RetentionPolicyKind::RecentBytes,
            ]),
            Arc::from([
                CompletionPolicyKind::UntilStopped,
                CompletionPolicyKind::DurationAfterOrigin,
                CompletionPolicyKind::SamplesAfterOrigin,
            ]),
            TriggerPlacementCapability::Fixed(TriggerPlacement::Fraction(
                CaptureFraction::new(0).expect("zero is a valid fraction"),
            )),
            Arc::from([
                TriggerTimeoutAction::ContinueWaiting,
                TriggerTimeoutAction::Stop,
                TriggerTimeoutAction::ForceTrigger,
            ]),
        )
        .expect("default continuous policy capabilities are valid")
    }

    pub fn recording_starts(&self) -> &[RecordingStart] {
        &self.recording_starts
    }

    pub fn retention(&self) -> &[RetentionPolicyKind] {
        &self.retention
    }

    pub fn completion(&self) -> &[CompletionPolicyKind] {
        &self.completion
    }

    pub const fn trigger_placement(&self) -> TriggerPlacementCapability {
        self.trigger_placement
    }

    pub fn trigger_timeout_actions(&self) -> &[TriggerTimeoutAction] {
        &self.trigger_timeout_actions
    }

    pub fn negotiate(
        &self,
        requested: &CapturePolicy,
        context: CapturePolicyContext,
    ) -> Result<EffectiveCapturePolicy, CapturePolicyError> {
        if !self.recording_starts.contains(&requested.start) {
            return Err(CapturePolicyError::Unsupported(format!(
                "recording start {:?} is not supported",
                requested.start
            )));
        }
        if requested.start == RecordingStart::Trigger && !context.has_trigger_program {
            return Err(CapturePolicyError::Invalid(
                "triggered recording requires an active trigger program".into(),
            ));
        }
        for retention in [
            requested.retention_before_origin,
            requested.retention_after_origin,
        ] {
            if !self.retention.contains(&retention.into()) {
                return Err(CapturePolicyError::Unsupported(format!(
                    "retention policy {retention:?} is not supported"
                )));
            }
            match retention {
                RetentionPolicy::RecentDuration(duration) if duration.is_zero() => {
                    return Err(CapturePolicyError::Invalid(
                        "retained duration must be non-zero".into(),
                    ));
                }
                RetentionPolicy::RecentBytes(0) => {
                    return Err(CapturePolicyError::Invalid(
                        "retained byte count must be non-zero".into(),
                    ));
                }
                _ => {}
            }
        }
        if !self.completion.contains(&requested.completion.into()) {
            return Err(CapturePolicyError::Unsupported(format!(
                "completion policy {:?} is not supported",
                requested.completion
            )));
        }
        match requested.completion {
            CompletionPolicy::DurationAfterOrigin(duration) if duration.is_zero() => {
                return Err(CapturePolicyError::Invalid(
                    "completion duration must be non-zero".into(),
                ));
            }
            CompletionPolicy::SamplesAfterOrigin(0) => {
                return Err(CapturePolicyError::Invalid(
                    "completion sample count must be non-zero".into(),
                ));
            }
            _ => {}
        }
        if let Some(timeout) = requested.trigger_timeout {
            if requested.start != RecordingStart::Trigger {
                return Err(CapturePolicyError::Invalid(
                    "a trigger timeout requires triggered recording".into(),
                ));
            }
            if timeout.after.is_zero() {
                return Err(CapturePolicyError::Invalid(
                    "trigger timeout must be non-zero".into(),
                ));
            }
            if !self.trigger_timeout_actions.contains(&timeout.action) {
                return Err(CapturePolicyError::Unsupported(format!(
                    "trigger-timeout action {:?} is not supported",
                    timeout.action
                )));
            }
        }

        let trigger_placement = self.negotiate_trigger_placement(requested, context)?;
        let mut effective = requested.clone();
        effective.trigger_placement = trigger_placement;
        Ok(EffectiveCapturePolicy {
            requested: requested.clone(),
            effective,
        })
    }

    fn negotiate_trigger_placement(
        &self,
        requested: &CapturePolicy,
        context: CapturePolicyContext,
    ) -> Result<Option<TriggerPlacement>, CapturePolicyError> {
        if requested.start == RecordingStart::Immediate {
            if requested.trigger_placement.is_some() {
                return Err(CapturePolicyError::Invalid(
                    "immediate recording cannot request trigger placement".into(),
                ));
            }
            return Ok(None);
        }
        match self.trigger_placement {
            TriggerPlacementCapability::Unsupported => {
                if requested.trigger_placement.is_some() {
                    Err(CapturePolicyError::Unsupported(
                        "trigger placement is not supported".into(),
                    ))
                } else {
                    Ok(None)
                }
            }
            TriggerPlacementCapability::Fixed(fixed) => {
                if requested
                    .trigger_placement
                    .is_some_and(|requested| requested != fixed)
                {
                    return Err(CapturePolicyError::Unsupported(format!(
                        "trigger placement is fixed at {fixed:?}"
                    )));
                }
                Ok(Some(fixed))
            }
            TriggerPlacementCapability::SelectableFraction {
                minimum,
                maximum,
                step,
                sample_alignment,
            } => {
                let requested = requested.trigger_placement.ok_or_else(|| {
                    CapturePolicyError::Invalid(
                        "triggered recording requires a trigger placement".into(),
                    )
                })?;
                let window = context.capture_window_samples.ok_or_else(|| {
                    CapturePolicyError::Invalid(
                        "selectable trigger placement requires a finite capture window".into(),
                    )
                })?;
                if window == 0 || context.sample_rate_hz == 0 {
                    return Err(CapturePolicyError::Invalid(
                        "capture window and sample rate must be non-zero".into(),
                    ));
                }
                let requested_samples = match requested {
                    TriggerPlacement::Fraction(fraction) => fraction.samples_of(window),
                    TriggerPlacement::SamplesBefore(samples) => samples,
                    TriggerPlacement::DurationBefore(duration) => {
                        duration_to_samples(duration, context.sample_rate_hz)?
                    }
                };
                if requested_samples > window {
                    return Err(CapturePolicyError::Invalid(
                        "trigger placement exceeds the finite capture window".into(),
                    ));
                }
                let mut fraction_parts = ((u128::from(requested_samples)
                    * u128::from(CaptureFraction::DENOMINATOR))
                    / u128::from(window)) as u16;
                if fraction_parts < minimum.parts() || fraction_parts > maximum.parts() {
                    return Err(CapturePolicyError::Unsupported(format!(
                        "trigger placement lies outside {}..={} parts",
                        minimum.parts(),
                        maximum.parts()
                    )));
                }
                let relative = fraction_parts - minimum.parts();
                fraction_parts = minimum.parts() + relative.div_ceil(step.parts()) * step.parts();
                fraction_parts = fraction_parts.min(maximum.parts());
                let fraction = CaptureFraction::new(fraction_parts)?;
                let aligned_samples = fraction
                    .samples_of(window)
                    .div_ceil(sample_alignment)
                    .saturating_mul(sample_alignment)
                    .min(window);
                Ok(Some(TriggerPlacement::SamplesBefore(aligned_samples)))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapturePolicyContext {
    pub sample_rate_hz: u64,
    pub capture_window_samples: Option<u64>,
    pub has_trigger_program: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct EffectiveCapturePolicy {
    pub requested: CapturePolicy,
    pub effective: CapturePolicy,
}

impl EffectiveCapturePolicy {
    pub fn capture_now(&self) -> Self {
        let mut effective = self.effective.clone();
        effective.completion = match (effective.trigger_placement, effective.completion) {
            (
                Some(TriggerPlacement::SamplesBefore(before)),
                CompletionPolicy::SamplesAfterOrigin(after),
            ) => CompletionPolicy::SamplesAfterOrigin(before.saturating_add(after)),
            (
                Some(TriggerPlacement::DurationBefore(before)),
                CompletionPolicy::DurationAfterOrigin(after),
            ) => CompletionPolicy::DurationAfterOrigin(before.saturating_add(after)),
            _ => effective.completion,
        };
        effective.start = RecordingStart::Immediate;
        effective.trigger_placement = None;
        effective.trigger_timeout = None;
        Self {
            requested: self.requested.clone(),
            effective,
        }
    }

    pub fn completion_sample(
        &self,
        origin_sample: u64,
        sample_rate_hz: u64,
    ) -> Result<Option<u64>, CapturePolicyError> {
        let after_origin = match self.effective.completion {
            CompletionPolicy::UntilStopped => return Ok(None),
            CompletionPolicy::DurationAfterOrigin(duration) => {
                duration_to_samples(duration, sample_rate_hz)?
            }
            CompletionPolicy::SamplesAfterOrigin(samples) => samples,
        };
        origin_sample
            .checked_add(after_origin)
            .map(Some)
            .ok_or_else(|| CapturePolicyError::Invalid("completion sample overflows u64".into()))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureStartMode {
    #[default]
    SavedPolicy,
    CaptureNow,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CaptureSessionPlan {
    pub sample_rate_hz: u64,
    pub channel_count: usize,
    #[serde(default)]
    pub capture_window_samples: Option<u64>,
    pub policy: EffectiveCapturePolicy,
}

impl CaptureSessionPlan {
    pub fn capture_now(mut self) -> Self {
        self.policy = self.policy.capture_now();
        self
    }

    pub fn with_actual_trigger_sample(mut self, sample: u64) -> Self {
        if self.policy.effective.start != RecordingStart::Trigger {
            return self;
        }
        let total_samples = match (
            self.policy.effective.trigger_placement,
            self.policy.effective.completion,
        ) {
            (
                Some(TriggerPlacement::SamplesBefore(before)),
                CompletionPolicy::SamplesAfterOrigin(after),
            ) => Some(before.saturating_add(after)),
            _ => None,
        };
        self.policy.effective.trigger_placement = Some(TriggerPlacement::SamplesBefore(sample));
        if let Some(total_samples) = total_samples {
            self.policy.effective.completion =
                CompletionPolicy::SamplesAfterOrigin(total_samples.saturating_sub(sample).max(1));
        }
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureRetentionPin(u64);

#[derive(Debug)]
pub struct CaptureRetentionTracker {
    sample_rate_hz: u64,
    before: RetentionPolicy,
    after: RetentionPolicy,
    retained_start_sample: u64,
    next_pin: u64,
    pins: BTreeMap<CaptureRetentionPin, u64>,
}

impl CaptureRetentionTracker {
    pub fn new(
        sample_rate_hz: u64,
        before: RetentionPolicy,
        after: RetentionPolicy,
    ) -> Result<Self, CapturePolicyError> {
        if sample_rate_hz == 0 {
            return Err(CapturePolicyError::Invalid(
                "retention tracking requires a non-zero sample rate".into(),
            ));
        }
        Ok(Self {
            sample_rate_hz,
            before,
            after,
            retained_start_sample: 0,
            next_pin: 1,
            pins: BTreeMap::new(),
        })
    }

    pub fn pin_from(&mut self, sample: u64) -> CaptureRetentionPin {
        let pin = CaptureRetentionPin(self.next_pin);
        self.next_pin = self.next_pin.saturating_add(1);
        self.pins.insert(pin, sample);
        pin
    }

    pub fn update_pin(&mut self, pin: CaptureRetentionPin, sample: u64) -> bool {
        let Some(current) = self.pins.get_mut(&pin) else {
            return false;
        };
        *current = sample;
        true
    }

    pub fn unpin(&mut self, pin: CaptureRetentionPin) -> bool {
        self.pins.remove(&pin).is_some()
    }

    pub fn safe_reclaim_before(
        &self,
        committed_samples: u64,
        committed_bytes: u64,
        recording_origin: Option<u64>,
    ) -> u64 {
        let policy_boundary = match recording_origin {
            None => retention_boundary(
                self.before,
                committed_samples,
                committed_samples,
                committed_bytes,
                self.sample_rate_hz,
            ),
            Some(0) => retention_boundary(
                self.after,
                committed_samples,
                committed_samples,
                committed_bytes,
                self.sample_rate_hz,
            ),
            Some(origin) => {
                let before = retention_boundary(
                    self.before,
                    origin,
                    committed_samples,
                    committed_bytes,
                    self.sample_rate_hz,
                );
                let after = origin.saturating_add(retention_boundary(
                    self.after,
                    committed_samples.saturating_sub(origin),
                    committed_samples,
                    committed_bytes,
                    self.sample_rate_hz,
                ));
                before.min(after)
            }
        };
        self.pins
            .values()
            .copied()
            .min()
            .map_or(policy_boundary, |pinned| policy_boundary.min(pinned))
            .max(self.retained_start_sample)
    }

    pub fn record_reclaimed_to(
        &mut self,
        requested: u64,
        committed_samples: u64,
        committed_bytes: u64,
        recording_origin: Option<u64>,
    ) -> Result<(), CapturePolicyError> {
        let safe = self.safe_reclaim_before(committed_samples, committed_bytes, recording_origin);
        if requested < self.retained_start_sample || requested > safe {
            return Err(CapturePolicyError::UnsafeReclamation { requested, safe });
        }
        self.retained_start_sample = requested;
        Ok(())
    }

    pub const fn retained_start_sample(&self) -> u64 {
        self.retained_start_sample
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CapturePolicyError {
    #[error("invalid capture policy: {0}")]
    Invalid(String),
    #[error("unsupported capture policy: {0}")]
    Unsupported(String),
    #[error("cannot reclaim to sample {requested}; the safe boundary is {safe}")]
    UnsafeReclamation { requested: u64, safe: u64 },
}

fn duration_to_samples(duration: Duration, sample_rate_hz: u64) -> Result<u64, CapturePolicyError> {
    let samples = duration
        .as_nanos()
        .checked_mul(u128::from(sample_rate_hz))
        .ok_or_else(|| CapturePolicyError::Invalid("duration-to-samples overflow".into()))?
        .div_ceil(1_000_000_000);
    u64::try_from(samples).map_err(|_| CapturePolicyError::Invalid("duration is too long".into()))
}

fn retention_boundary(
    policy: RetentionPolicy,
    extent_samples: u64,
    committed_samples: u64,
    committed_bytes: u64,
    sample_rate_hz: u64,
) -> u64 {
    let retained_samples = match policy {
        RetentionPolicy::Everything | RetentionPolicy::DeviceManaged => return 0,
        RetentionPolicy::RecentDuration(duration) => {
            duration_to_samples(duration, sample_rate_hz).unwrap_or(u64::MAX)
        }
        RetentionPolicy::RecentBytes(bytes) => {
            if committed_bytes == 0 {
                return 0;
            }
            ((u128::from(bytes) * u128::from(committed_samples)) / u128::from(committed_bytes))
                as u64
        }
    };
    extent_samples.saturating_sub(retained_samples)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{
        CaptureFraction, CapturePolicy, CapturePolicyCapabilities, CapturePolicyContext,
        CapturePolicyError, CaptureRetentionTracker, CaptureSessionPlan, CompletionPolicy,
        CompletionPolicyKind, RecordingStart, RetentionPolicy, RetentionPolicyKind,
        TriggerPlacement, TriggerPlacementCapability, TriggerTimeout, TriggerTimeoutAction,
    };

    fn selectable() -> CapturePolicyCapabilities {
        CapturePolicyCapabilities::new(
            Arc::from([RecordingStart::Immediate, RecordingStart::Trigger]),
            Arc::from([
                RetentionPolicyKind::Everything,
                RetentionPolicyKind::RecentDuration,
                RetentionPolicyKind::RecentBytes,
            ]),
            Arc::from([
                CompletionPolicyKind::UntilStopped,
                CompletionPolicyKind::SamplesAfterOrigin,
            ]),
            TriggerPlacementCapability::SelectableFraction {
                minimum: CaptureFraction::from_percent(10).unwrap(),
                maximum: CaptureFraction::from_percent(90).unwrap(),
                step: CaptureFraction::from_percent(1).unwrap(),
                sample_alignment: 64,
            },
            Arc::from([
                TriggerTimeoutAction::ContinueWaiting,
                TriggerTimeoutAction::Stop,
                TriggerTimeoutAction::ForceTrigger,
            ]),
        )
        .unwrap()
    }

    #[test]
    fn selectable_trigger_placement_converts_duration_and_aligns_samples() {
        let policy = CapturePolicy {
            start: RecordingStart::Trigger,
            trigger_placement: Some(TriggerPlacement::DurationBefore(Duration::from_millis(250))),
            retention_before_origin: RetentionPolicy::Everything,
            retention_after_origin: RetentionPolicy::Everything,
            completion: CompletionPolicy::SamplesAfterOrigin(750),
            trigger_timeout: None,
        };
        let effective = selectable()
            .negotiate(
                &policy,
                CapturePolicyContext {
                    sample_rate_hz: 1_000,
                    capture_window_samples: Some(1_000),
                    has_trigger_program: true,
                },
            )
            .unwrap();

        assert_eq!(
            effective.effective.trigger_placement,
            Some(TriggerPlacement::SamplesBefore(256))
        );
        assert_eq!(effective.requested, policy);
        let immediate = effective.capture_now();
        assert_eq!(immediate.effective.start, RecordingStart::Immediate);
        assert_eq!(
            immediate.effective.completion,
            CompletionPolicy::SamplesAfterOrigin(1_006)
        );
        let actual = CaptureSessionPlan {
            sample_rate_hz: 1_000,
            channel_count: 1,
            capture_window_samples: Some(1_000),
            policy: effective,
        }
        .with_actual_trigger_sample(200);
        assert_eq!(
            actual.policy.effective.trigger_placement,
            Some(TriggerPlacement::SamplesBefore(200))
        );
        assert_eq!(
            actual.policy.effective.completion,
            CompletionPolicy::SamplesAfterOrigin(806)
        );
    }

    #[test]
    fn invalid_and_unsupported_policy_compositions_are_rejected() {
        let context = CapturePolicyContext {
            sample_rate_hz: 1_000,
            capture_window_samples: Some(1_000),
            has_trigger_program: false,
        };
        let mut policy = CapturePolicy {
            start: RecordingStart::Trigger,
            trigger_placement: Some(TriggerPlacement::Fraction(CaptureFraction::default())),
            completion: CompletionPolicy::SamplesAfterOrigin(500),
            ..CapturePolicy::default()
        };
        assert!(matches!(
            selectable().negotiate(&policy, context),
            Err(CapturePolicyError::Invalid(_))
        ));

        policy.start = RecordingStart::Immediate;
        policy.trigger_placement = None;
        policy.retention_after_origin = RetentionPolicy::DeviceManaged;
        assert!(matches!(
            selectable().negotiate(&policy, context),
            Err(CapturePolicyError::Unsupported(_))
        ));

        policy.retention_after_origin = RetentionPolicy::Everything;
        policy.trigger_timeout = Some(TriggerTimeout {
            after: Duration::from_secs(1),
            action: TriggerTimeoutAction::Stop,
        });
        assert!(matches!(
            selectable().negotiate(&policy, context),
            Err(CapturePolicyError::Invalid(_))
        ));
    }

    #[test]
    fn pins_bound_reclamation_until_every_consumer_advances() {
        let mut tracker = CaptureRetentionTracker::new(
            1_000,
            RetentionPolicy::RecentDuration(Duration::from_secs(2)),
            RetentionPolicy::RecentDuration(Duration::from_secs(2)),
        )
        .unwrap();
        let viewer = tracker.pin_from(500);
        let graph = tracker.pin_from(800);

        assert_eq!(tracker.safe_reclaim_before(5_000, 5_000, Some(0)), 500);
        assert_eq!(
            tracker.record_reclaimed_to(501, 5_000, 5_000, Some(0)),
            Err(CapturePolicyError::UnsafeReclamation {
                requested: 501,
                safe: 500,
            })
        );
        assert!(tracker.update_pin(viewer, 3_500));
        assert!(tracker.update_pin(graph, 3_200));
        assert_eq!(tracker.safe_reclaim_before(5_000, 5_000, Some(0)), 3_000);
        tracker
            .record_reclaimed_to(3_000, 5_000, 5_000, Some(0))
            .unwrap();
        assert!(tracker.unpin(viewer));
        assert!(tracker.unpin(graph));
        assert_eq!(tracker.retained_start_sample(), 3_000);

        let triggered = CaptureRetentionTracker::new(
            1_000,
            RetentionPolicy::RecentDuration(Duration::from_millis(500)),
            RetentionPolicy::RecentDuration(Duration::from_secs(2)),
        )
        .unwrap();
        assert_eq!(
            triggered.safe_reclaim_before(5_000, 5_000, Some(2_000)),
            1_500,
            "the preserved pre-trigger window remains the earliest required range"
        );
    }
}
