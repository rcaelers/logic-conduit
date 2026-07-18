use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use tempfile::TempDir;

use logic_analyzer_graph::compiler::{CaptureGraphSourceFactory, DiscoveredLiveCaptureFeature};
use logic_analyzer_processing::AcquisitionContext;
use signal_processing::{
    CaptureAcquisitionPhase, CaptureCompletion, CaptureEvent, CaptureEventPublishError,
    CaptureEventPublisher, CaptureEventQueueReader, CaptureHealth, CaptureIndex, CaptureProgress,
    CaptureQueueReceiveError, CaptureRecordingGate, CaptureSessionId, CaptureSessionPlan,
    CaptureSessionState, CaptureStartMode, CaptureStoreDescriptor, NativeCaptureStore,
    NativeCaptureStoreConfig, NativeFinalizedCapture, RecordingStart, TriggerTimeoutAction,
    bounded_capture_event_queue,
};
use signal_processing::live_capture_waveform::NativeGrowingCaptureIndex;

use super::{
    CaptureAnalysisAttachment, CaptureCoordinatorContract, CaptureReplayAttachment,
    CaptureSessionStatus, CaptureWaveformUpdate,
};

const EVENT_QUEUE_CAPACITY: usize = 1_024;
const SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_millis(5);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

enum CaptureCommand {
    Stop,
    Abort,
    ForceTrigger,
}

struct CompletedCapture {
    _directory: TempDir,
    capture: NativeFinalizedCapture,
    waveform: NativeGrowingCaptureIndex,
    source_node: node_graph::NodeId,
    graph_source_factory: Arc<dyn CaptureGraphSourceFactory>,
    recording_origin: Option<u64>,
    session_plan: Option<CaptureSessionPlan>,
    completion: CaptureCompletion,
}

#[derive(Default)]
struct CaptureRuntimeSignals {
    trigger_sample: Option<u64>,
    captured_samples: u64,
}

struct RecordingEventPublisher {
    inner: Box<dyn CaptureEventPublisher>,
    recording_gate: CaptureRecordingGate,
    waveform: NativeGrowingCaptureIndex,
    store: NativeCaptureStore,
    runtime: Arc<Mutex<CaptureRuntimeSignals>>,
    last_health_at: Instant,
    last_health_bytes: u64,
}

impl CaptureEventPublisher for RecordingEventPublisher {
    fn publish(&mut self, event: CaptureEvent) -> Result<(), CaptureEventPublishError> {
        let progress = match &event {
            CaptureEvent::Triggered { sample, .. } => {
                self.recording_gate.resolve_trigger(*sample);
                self.waveform.set_trigger_sample(*sample);
                self.runtime
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .trigger_sample = Some(*sample);
                None
            }
            CaptureEvent::Progress { progress, .. } => {
                if let Some(samples) = progress.captured_samples {
                    self.runtime
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .captured_samples = samples;
                }
                Some(*progress)
            }
            _ => None,
        };
        self.inner.publish(event)?;
        let elapsed = self.last_health_at.elapsed();
        if let Some(progress) = progress
            && elapsed >= Duration::from_millis(100)
        {
            let transferred = progress.transferred_bytes.unwrap_or(self.last_health_bytes);
            let bytes = transferred.saturating_sub(self.last_health_bytes);
            let rate = u64::try_from(
                (u128::from(bytes) * 1_000_000_000_u128)
                    / elapsed.as_nanos().max(1),
            )
            .unwrap_or(u64::MAX);
            let snapshot = self.store.snapshot();
            let indexed = self.waveform.current_metadata().total_samples;
            let _ = self.inner.publish(CaptureEvent::Health {
                session_id: self.store.descriptor().session_id(),
                health: CaptureHealth {
                    input_bytes_per_second: Some(rate),
                    write_bytes_per_second: Some(rate),
                    retained_samples: Some(snapshot.committed_samples),
                    summary_lag_samples: Some(
                        snapshot.committed_samples.saturating_sub(indexed),
                    ),
                    ..CaptureHealth::default()
                },
            });
            self.last_health_at = Instant::now();
            self.last_health_bytes = transferred;
        }
        Ok(())
    }
}

enum WorkerCompletion {
    Complete(Box<CompletedCapture>),
    Failed(String),
}

struct ActiveCapture {
    commands: Sender<CaptureCommand>,
    completion: Receiver<WorkerCompletion>,
    waveforms: Receiver<NativeGrowingCaptureIndex>,
    analyses: Receiver<CaptureAnalysisAttachment>,
    events: CaptureEventQueueReader,
    worker: Option<JoinHandle<()>>,
    stop_requested: bool,
    abort_requested: bool,
}

pub(crate) struct CaptureCoordinator {
    status: Option<CaptureSessionStatus>,
    active: Option<ActiveCapture>,
    completed: Option<CompletedCapture>,
    waveform_update: Option<CaptureWaveformUpdate>,
    analysis_attachment: Option<CaptureAnalysisAttachment>,
    state_history: Vec<CaptureSessionState>,
}

impl CaptureCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            status: None,
            active: None,
            completed: None,
            waveform_update: None,
            analysis_attachment: None,
            state_history: Vec::new(),
        }
    }

    fn record_state(&mut self, state: CaptureSessionState) {
        if self.state_history.last().copied() != Some(state) {
            self.state_history.push(state);
        }
    }

    pub(crate) fn clear_completed(&mut self) {
        self.completed = None;
        self.status = None;
        self.waveform_update = Some(None);
    }

    fn apply_event(&mut self, event: CaptureEvent) -> bool {
        let Some(status) = &mut self.status else {
            return false;
        };
        let mut hold_triggered_state = false;
        match event {
            CaptureEvent::Status(event) if event.session_id == status.session_id => {
                let stop_requested = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.stop_requested);
                if stop_requested
                    && !matches!(
                        event.state,
                        CaptureSessionState::Stopping
                            | CaptureSessionState::Complete
                            | CaptureSessionState::Error
                    )
                {
                    return false;
                }
                status.state = event.state;
                status.phase = event.phase;
                self.record_state(event.state);
            }
            CaptureEvent::Progress {
                session_id,
                progress,
            } if session_id == status.session_id => status.progress = progress,
            CaptureEvent::Health { session_id, health }
                if session_id == status.session_id =>
            {
                status.health = health;
            }
            CaptureEvent::Plan { session_id, plan } if session_id == status.session_id => {
                status.session_plan = Some(plan);
            }
            CaptureEvent::Triggered { session_id, sample }
                if session_id == status.session_id =>
            {
                status.state = CaptureSessionState::Triggered;
                status.trigger_sample = Some(sample);
                status.recording_origin = Some(sample);
                status.session_plan = status
                    .session_plan
                    .take()
                    .map(|plan| plan.with_actual_trigger_sample(sample));
                self.record_state(CaptureSessionState::Triggered);
                hold_triggered_state = true;
            }
            CaptureEvent::Failed(failure) if failure.session_id == status.session_id => {
                status.state = CaptureSessionState::Error;
                status.phase = CaptureAcquisitionPhase::Finalizing;
                status.error = Some(failure.message);
                self.record_state(CaptureSessionState::Error);
            }
            _ => {}
        }
        hold_triggered_state
    }

    fn finish_worker(&mut self, completion: WorkerCompletion) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        if let Some(worker) = active.worker.take() {
            let _ = worker.join();
        }
        match completion {
            WorkerCompletion::Complete(capture) => {
                if let Some(status) = &mut self.status {
                    status.state = CaptureSessionState::Complete;
                    status.phase = CaptureAcquisitionPhase::Finalizing;
                    status.session_plan = capture.session_plan.clone();
                    status.completion = Some(capture.completion);
                }
                self.record_state(CaptureSessionState::Complete);
                self.completed = Some(*capture);
            }
            WorkerCompletion::Failed(error) => {
                if let Some(status) = &mut self.status {
                    status.state = CaptureSessionState::Error;
                    status.phase = CaptureAcquisitionPhase::Finalizing;
                    status.error = Some(error);
                }
                self.record_state(CaptureSessionState::Error);
                self.waveform_update = Some(match &self.completed {
                    Some(completed) => Some(Box::new(completed.waveform.clone())),
                    None => None,
                });
            }
        }
    }

    #[cfg(test)]
    fn completed_manifest(&self) -> Option<signal_processing::CaptureStoreManifest> {
        self.completed
            .as_ref()
            .map(|completed| completed.capture.manifest())
    }

    #[cfg(test)]
    fn completed_recording_origin(&self) -> Option<u64> {
        self.completed
            .as_ref()
            .and_then(|completed| completed.recording_origin)
    }

    #[cfg(test)]
    fn completed_trigger_sample(&self) -> Option<u64> {
        self.completed.as_ref().and_then(|completed| {
            signal_processing::CaptureIndex::current_metadata(&completed.waveform).trigger_sample
        })
    }

    #[cfg(test)]
    fn completed_session_plan(&self) -> Option<&CaptureSessionPlan> {
        self.completed
            .as_ref()
            .and_then(|completed| completed.session_plan.as_ref())
    }

    #[cfg(test)]
    fn completed_persisted_session_plan(&self) -> Option<CaptureSessionPlan> {
        self.completed
            .as_ref()
            .and_then(|completed| completed.capture.session_plan().ok().flatten())
    }

    #[cfg(test)]
    fn state_history(&self) -> &[CaptureSessionState] {
        &self.state_history
    }
}

impl CaptureCoordinatorContract for CaptureCoordinator {
    fn backend_available() -> bool {
        true
    }

    fn backend_unavailable_reason() -> &'static str {
        ""
    }

    fn start(
        &mut self,
        feature: DiscoveredLiveCaptureFeature,
        mode: CaptureStartMode,
    ) -> Result<(), String> {
        if self.is_active() {
            return Err("a live capture is already active".into());
        }
        let commands = feature.capabilities().commands();
        if mode == CaptureStartMode::CaptureNow && !commands.capture_now {
            return Err("this capture source does not support Capture Now".into());
        }
        let session_id = fresh_session_id();
        let source_node = feature.source_node;
        let source_title = feature.source_title.clone();
        let session_plan = feature.session_plan().cloned().map(|plan| {
            if mode == CaptureStartMode::CaptureNow {
                plan.capture_now()
            } else {
                plan
            }
        });
        let immediate_recording_origin = session_plan
            .as_ref()
            .map(|plan| plan.policy.effective.start == RecordingStart::Immediate)
            .unwrap_or_else(|| !feature.has_simple_trigger())
            .then_some(0);
        let (event_publisher, events) = bounded_capture_event_queue(EVENT_QUEUE_CAPACITY)
            .expect("capture event queue capacity is non-zero");
        let (command_sender, command_receiver) = crossbeam_channel::bounded(1);
        let (completion_sender, completion_receiver) = crossbeam_channel::bounded(1);
        let (waveform_sender, waveform_receiver) = crossbeam_channel::bounded(1);
        let (analysis_sender, analysis_receiver) = crossbeam_channel::bounded(1);
        let worker = std::thread::Builder::new()
            .name("live-capture-supervisor".into())
            .spawn(move || {
                let completion = match run_capture_worker(
                    session_id,
                    feature,
                    Box::new(event_publisher),
                    command_receiver,
                    waveform_sender,
                    analysis_sender,
                    mode,
                ) {
                    Ok(capture) => WorkerCompletion::Complete(Box::new(capture)),
                    Err(error) => WorkerCompletion::Failed(error),
                };
                let _ = completion_sender.send(completion);
            })
            .map_err(|error| format!("could not start capture supervisor: {error}"))?;

        self.status = Some(CaptureSessionStatus {
            session_id,
            source_node,
            source_title,
            state: CaptureSessionState::Preparing,
            phase: CaptureAcquisitionPhase::Preparing,
            progress: CaptureProgress::default(),
            health: CaptureHealth::default(),
            commands,
            session_plan,
            trigger_sample: None,
            recording_origin: immediate_recording_origin,
            completion: None,
            error: None,
        });
        self.state_history.clear();
        self.record_state(CaptureSessionState::Preparing);
        self.active = Some(ActiveCapture {
            commands: command_sender,
            completion: completion_receiver,
            waveforms: waveform_receiver,
            analyses: analysis_receiver,
            events,
            worker: Some(worker),
            stop_requested: false,
            abort_requested: false,
        });
        Ok(())
    }

    fn request_stop(&mut self) {
        if self
            .status
            .as_ref()
            .is_some_and(|status| !status.commands.orderly_stop)
        {
            return;
        }
        let Some(active) = &mut self.active else {
            return;
        };
        if active.stop_requested {
            return;
        }
        active.stop_requested = true;
        let _ = active.commands.try_send(CaptureCommand::Stop);
        if let Some(status) = &mut self.status {
            status.state = CaptureSessionState::Stopping;
            status.phase = CaptureAcquisitionPhase::Finalizing;
        }
        self.record_state(CaptureSessionState::Stopping);
    }

    fn request_abort(&mut self) -> Result<(), String> {
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| "there is no active capture to abort".to_owned())?;
        let status = self
            .status
            .as_ref()
            .ok_or_else(|| "capture status is unavailable".to_owned())?;
        if !status.commands.abort {
            return Err("this capture source does not support Abort".into());
        }
        if !active.abort_requested {
            active.abort_requested = true;
            active
                .commands
                .try_send(CaptureCommand::Abort)
                .map_err(|error| format!("could not request capture abort: {error}"))?;
        }
        Ok(())
    }

    fn request_force_trigger(&mut self) -> Result<(), String> {
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| "there is no armed capture".to_owned())?;
        let status = self
            .status
            .as_ref()
            .ok_or_else(|| "capture status is unavailable".to_owned())?;
        if status.state != CaptureSessionState::Armed {
            return Err("Force Trigger is available only while capture is armed".into());
        }
        if !status.commands.force_trigger {
            return Err("this capture source does not support Force Trigger".into());
        }
        active
            .commands
            .try_send(CaptureCommand::ForceTrigger)
            .map_err(|error| format!("could not request force trigger: {error}"))
    }

    fn set_graph_processed_samples(&mut self, processed_samples: Option<u64>) {
        let Some(status) = &mut self.status else {
            return;
        };
        status.health.graph_lag_samples = processed_samples.and_then(|processed| {
            status
                .recording_origin
                .zip(status.progress.captured_samples)
                .map(|(origin, captured)| captured.saturating_sub(origin).saturating_sub(processed))
        });
    }

    fn poll(&mut self) {
        if let Some(analysis) = self
            .active
            .as_ref()
            .and_then(|active| active.analyses.try_recv().ok())
        {
            self.analysis_attachment = Some(analysis);
        }
        if let Some(waveform) = self
            .active
            .as_ref()
            .and_then(|active| active.waveforms.try_recv().ok())
        {
            self.waveform_update = Some(Some(Box::new(waveform)));
        }
        let mut hold_triggered_state = false;
        loop {
            let event = self
                .active
                .as_ref()
                .map(|active| active.events.try_recv());
            match event {
                Some(Ok(event)) => {
                    hold_triggered_state = self.apply_event(event);
                    if hold_triggered_state {
                        break;
                    }
                }
                Some(Err(CaptureQueueReceiveError::Empty | CaptureQueueReceiveError::Closed))
                | None => break,
                Some(Err(CaptureQueueReceiveError::Timeout)) => unreachable!(),
            }
        }

        if hold_triggered_state {
            return;
        }

        let completion = self
            .active
            .as_ref()
            .map(|active| active.completion.try_recv());
        match completion {
            Some(Ok(completion)) => self.finish_worker(completion),
            Some(Err(TryRecvError::Disconnected)) => self.finish_worker(WorkerCompletion::Failed(
                "capture supervisor stopped without a result".into(),
            )),
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    fn status(&self) -> Option<&CaptureSessionStatus> {
        self.status.as_ref()
    }

    fn take_waveform_update(&mut self) -> Option<CaptureWaveformUpdate> {
        self.waveform_update.take()
    }

    fn take_analysis_attachment(&mut self) -> Option<CaptureAnalysisAttachment> {
        self.analysis_attachment.take()
    }

    fn replay_source_node(&self) -> Option<node_graph::NodeId> {
        self.completed.as_ref().map(|capture| capture.source_node)
    }

    fn create_replay_attachment(&self) -> Result<Option<CaptureReplayAttachment>, String> {
        let Some(completed) = &self.completed else {
            return Ok(None);
        };
        let cursor = completed
            .capture
            .open_cursor()
            .map_err(|error| format!("could not open finalized capture: {error}"))?;
        let cursor = CaptureRecordingGate::finalized(completed.recording_origin)
            .cursor(Box::new(cursor));
        let process = completed
            .graph_source_factory
            .create(Box::new(cursor))
            .map_err(|error| format!("could not build capture replay source: {error}"))?;
        Ok(Some(CaptureReplayAttachment {
            source_node: completed.source_node,
            process,
        }))
    }

    fn is_active(&self) -> bool {
        self.active.is_some()
    }
}

impl Drop for CaptureCoordinator {
    fn drop(&mut self) {
        if let Some(mut active) = self.active.take() {
            let _ = active.commands.try_send(CaptureCommand::Stop);
            if let Some(worker) = active.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

fn fresh_session_id() -> CaptureSessionId {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = u128::from(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed));
    CaptureSessionId::new(time.rotate_left(37) ^ sequence)
}

fn run_capture_worker(
    session_id: CaptureSessionId,
    feature: DiscoveredLiveCaptureFeature,
    events: Box<dyn signal_processing::CaptureEventPublisher>,
    commands: Receiver<CaptureCommand>,
    waveform_ready: Sender<NativeGrowingCaptureIndex>,
    analysis_ready: Sender<CaptureAnalysisAttachment>,
    mode: CaptureStartMode,
) -> Result<CompletedCapture, String> {
    let session_plan = feature.session_plan().cloned().map(|plan| {
        if mode == CaptureStartMode::CaptureNow {
            plan.capture_now()
        } else {
            plan
        }
    });
    let triggered_recording = session_plan
        .as_ref()
        .map(|plan| plan.policy.effective.start == RecordingStart::Trigger)
        .unwrap_or_else(|| feature.has_simple_trigger());
    let recording_gate = if triggered_recording {
        CaptureRecordingGate::pending()
    } else {
        CaptureRecordingGate::immediate()
    };
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let descriptor = CaptureStoreDescriptor::new(session_id, feature.channels().to_vec())
        .map_err(|error| error.to_string())?;
    let (store, writer) =
        NativeCaptureStore::create(NativeCaptureStoreConfig::new(directory.path(), descriptor))
            .map_err(|error| error.to_string())?;
    let graph_source_factory = feature.graph_source_factory();
    let analysis_cursor = store.open_cursor().map_err(|error| error.to_string())?;
    let analysis_cursor = recording_gate.cursor(Box::new(analysis_cursor));
    let analysis_process = graph_source_factory
        .create(Box::new(analysis_cursor))
        .map_err(|error| format!("could not build live analysis source: {error}"))?;
    analysis_ready
        .send(CaptureAnalysisAttachment {
            source_node: feature.source_node,
            process: analysis_process,
        })
        .map_err(|_| "live analysis attachment receiver closed".to_owned())?;
    let source_node = feature.source_node;
    let source_title = feature.source_title.clone();
    let (waveform, waveform_worker) = NativeGrowingCaptureIndex::spawn(
        store.clone(),
        source_title,
        feature.sample_rate_hz(),
        feature.channel_names().to_vec(),
    )
    .map_err(|error| error.to_string())?;
    let _ = waveform_ready.send(waveform.clone());
    let runtime = Arc::new(Mutex::new(CaptureRuntimeSignals::default()));
    let events = RecordingEventPublisher {
        inner: events,
        recording_gate: recording_gate.clone(),
        waveform: waveform.clone(),
        store: store.clone(),
        runtime: Arc::clone(&runtime),
        last_health_at: Instant::now(),
        last_health_bytes: 0,
    };
    let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
    let mut acquisition = feature
        .prepare(context, mode)
        .map_err(|error| error.to_string())?;
    acquisition.start().map_err(|error| error.to_string())?;

    let mut stop_requested = false;
    let mut abort_requested = false;
    let mut trigger_timeout = session_plan
        .as_ref()
        .and_then(|plan| plan.policy.effective.trigger_timeout)
        .map(|timeout| (Instant::now() + timeout.after, timeout.action));
    while !acquisition.is_finished() {
        match commands.recv_timeout(SUPERVISOR_POLL_INTERVAL) {
            Ok(CaptureCommand::Stop) if !stop_requested => {
                stop_requested = true;
                acquisition
                    .request_stop()
                    .map_err(|error| error.to_string())?;
            }
            Ok(CaptureCommand::Abort) if !abort_requested => {
                abort_requested = true;
                acquisition
                    .request_abort()
                    .map_err(|error| error.to_string())?;
            }
            Ok(CaptureCommand::ForceTrigger) => {
                acquisition
                    .request_force_trigger()
                    .map_err(|error| error.to_string())?;
            }
            Ok(CaptureCommand::Stop | CaptureCommand::Abort)
            | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) if !stop_requested => {
                stop_requested = true;
                acquisition
                    .request_stop()
                    .map_err(|error| error.to_string())?;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {}
        }

        let signals = runtime.lock().unwrap_or_else(|error| error.into_inner());
        let trigger_sample = signals.trigger_sample;
        let captured_samples = signals.captured_samples;
        drop(signals);
        if trigger_sample.is_some() {
            trigger_timeout = None;
        }
        if !stop_requested
            && let Some((deadline, action)) = trigger_timeout
            && Instant::now() >= deadline
        {
            trigger_timeout = None;
            match action {
                TriggerTimeoutAction::ContinueWaiting => {}
                TriggerTimeoutAction::Stop => {
                    stop_requested = true;
                    acquisition
                        .request_stop()
                        .map_err(|error| error.to_string())?;
                }
                TriggerTimeoutAction::ForceTrigger => {
                    acquisition
                        .request_force_trigger()
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        let origin = trigger_sample.or((!triggered_recording).then_some(0));
        if !stop_requested
            && let (Some(plan), Some(origin)) = (&session_plan, origin)
            && let Some(completion) = plan
                .policy
                .completion_sample(origin, plan.sample_rate_hz)
                .map_err(|error| error.to_string())?
            && captured_samples >= completion
        {
            stop_requested = true;
            acquisition
                .request_stop()
                .map_err(|error| error.to_string())?;
        }
    }
    let outcome = acquisition.join().map_err(|error| error.to_string())?;
    if !recording_gate.is_resolved() {
        recording_gate.finish_without_trigger();
    }
    waveform_worker.join().map_err(|error| error.to_string())?;
    let session_plan = session_plan.map(|plan| match recording_gate.recording_origin() {
        Some(sample) => plan.with_actual_trigger_sample(sample),
        None => plan,
    });
    if let Some(plan) = &session_plan {
        store
            .write_session_plan(plan)
            .map_err(|error| error.to_string())?;
    }
    let capture = store.finalize().map_err(|error| error.to_string())?;
    Ok(CompletedCapture {
        _directory: directory,
        capture,
        waveform,
        source_node,
        graph_source_factory,
        recording_origin: recording_gate.recording_origin(),
        session_plan,
        completion: outcome.completion,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use logic_analyzer_graph::compiler::{
        CaptureGraphSourceFactory, DiscoveredLiveCaptureFeature, LiveCaptureFeature,
        SimpleTriggerChannel,
    };
    use logic_analyzer_graph::{compiler, nodes};
    use logic_analyzer_processing::{
        AcquisitionContext, AcquisitionError, AcquisitionResult, CaptureAnalysisChannel,
        CaptureAnalysisSource, BufferedFakeConfig, BufferedFakeController, BufferedFakeProvider,
        DeterministicFakeConfig, DeterministicFakeController, DeterministicFakeProvider,
        PreparedAcquisition,
    };
    use node_graph::{NodeDef, NodeGraphWidget, NodeId};
    use signal_processing::{
        CaptureCapacityEstimate, CaptureChannelId, CaptureCommandCapabilities, CaptureDataDelivery,
        CapturePolicy, CaptureProviderCapabilities, CaptureSessionPlan, CaptureSessionState,
        CaptureStartMode, CaptureStoreCursor, CompletionPolicy, EffectiveCapturePolicy,
        ProcessNode, RecordingStart, RetentionPolicy, SimpleTriggerCondition, TriggerTimeout,
        TriggerTimeoutAction,
    };

    use super::{CaptureCoordinator, CaptureCoordinatorContract};

    type PrepareCapture = Box<
        dyn FnOnce(AcquisitionContext) -> AcquisitionResult<Box<dyn PreparedAcquisition>> + Send,
    >;

    struct FakeFeature {
        channels: Vec<CaptureChannelId>,
        channel_names: Vec<String>,
        sample_rate_hz: f64,
        prepare: Option<PrepareCapture>,
        prepare_calls: Arc<AtomicUsize>,
        simple_trigger_channels: Vec<SimpleTriggerChannel>,
        capabilities: CaptureProviderCapabilities,
        session_plan: Option<CaptureSessionPlan>,
    }

    struct TestGraphSourceFactory {
        channels: Vec<CaptureChannelId>,
        sample_rate_hz: f64,
    }

    impl CaptureGraphSourceFactory for TestGraphSourceFactory {
        fn create(
            &self,
            cursor: Box<dyn CaptureStoreCursor>,
        ) -> Result<Box<dyn ProcessNode>, String> {
            test_analysis_source(&self.channels, self.sample_rate_hz, cursor)
        }
    }

    impl LiveCaptureFeature for FakeFeature {
        fn channels(&self) -> &[CaptureChannelId] {
            &self.channels
        }

        fn channel_names(&self) -> &[String] {
            &self.channel_names
        }

        fn sample_rate_hz(&self) -> f64 {
            self.sample_rate_hz
        }

        fn capabilities(&self) -> &CaptureProviderCapabilities {
            &self.capabilities
        }

        fn simple_trigger_channels(&self) -> &[SimpleTriggerChannel] {
            &self.simple_trigger_channels
        }

        fn session_plan(&self) -> Option<&CaptureSessionPlan> {
            self.session_plan.as_ref()
        }

        fn graph_source_factory(&self) -> Arc<dyn CaptureGraphSourceFactory> {
            Arc::new(TestGraphSourceFactory {
                channels: self.channels.clone(),
                sample_rate_hz: self.sample_rate_hz,
            })
        }

        fn prepare(
            self: Box<Self>,
            context: AcquisitionContext,
        ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
            let mut feature = *self;
            feature.prepare_calls.fetch_add(1, Ordering::SeqCst);
            feature
                .prepare
                .take()
                .expect("test live-capture feature prepares at most once")(context)
        }
    }

    struct FailingFeature {
        channels: Vec<CaptureChannelId>,
        channel_names: Vec<String>,
        capabilities: CaptureProviderCapabilities,
    }

    impl LiveCaptureFeature for FailingFeature {
        fn channels(&self) -> &[CaptureChannelId] {
            &self.channels
        }

        fn channel_names(&self) -> &[String] {
            &self.channel_names
        }

        fn sample_rate_hz(&self) -> f64 {
            1_000_000_000.0
        }

        fn capabilities(&self) -> &CaptureProviderCapabilities {
            &self.capabilities
        }

        fn graph_source_factory(&self) -> Arc<dyn CaptureGraphSourceFactory> {
            Arc::new(TestGraphSourceFactory {
                channels: self.channels.clone(),
                sample_rate_hz: self.sample_rate_hz(),
            })
        }

        fn prepare(
            self: Box<Self>,
            _context: AcquisitionContext,
        ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
            Err(AcquisitionError::InvalidRequest(
                "intentional preparation failure".into(),
            ))
        }
    }

    fn test_analysis_source(
        channels: &[CaptureChannelId],
        sample_rate_hz: f64,
        cursor: Box<dyn CaptureStoreCursor>,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let layout = channels
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, channel)| {
                CaptureAnalysisChannel::polymorphic(channel, format!("ch{index}"))
            })
            .collect();
        CaptureAnalysisSource::new("test-live-analysis", cursor, sample_rate_hz, layout)
            .map(|source| Box::new(source) as Box<dyn ProcessNode>)
    }

    fn streaming_capabilities(channels: &[CaptureChannelId]) -> CaptureProviderCapabilities {
        CaptureProviderCapabilities::single(
            CaptureDataDelivery::DuringAcquisition,
            channels.to_vec(),
            1_000_000_000,
        )
        .with_commands(CaptureCommandCapabilities::new(true, true, true, true))
    }

    fn manual_feature_with_counter() -> (
        DiscoveredLiveCaptureFeature,
        DeterministicFakeController,
        Arc<AtomicUsize>,
    ) {
        let channels = vec![
            CaptureChannelId::new("bank-a:7"),
            CaptureChannelId::new("bank-c:2"),
        ];
        let config = DeterministicFakeConfig::new(channels.clone(), vec![3, 5, 2, 7], 0x5a17)
            .unwrap();
        let (provider, controller) = DeterministicFakeProvider::manually_paced(config);
        let prepare_calls = Arc::new(AtomicUsize::new(0));
        let capabilities = streaming_capabilities(&channels);
        let feature =
            DiscoveredLiveCaptureFeature::new(
                NodeId(41),
                "Contract Fake",
                Box::new(FakeFeature {
                    channel_names: vec!["Bank A 7".into(), "Bank C 2".into()],
                    channels,
                    sample_rate_hz: 1_000_000_000.0,
                    prepare: Some(Box::new(move |context| provider.prepare(context))),
                    prepare_calls: Arc::clone(&prepare_calls),
                    simple_trigger_channels: Vec::new(),
                    capabilities,
                    session_plan: None,
                }),
            );
        (feature, controller, prepare_calls)
    }

    fn manual_feature() -> (DiscoveredLiveCaptureFeature, DeterministicFakeController) {
        let (feature, controller, _) = manual_feature_with_counter();
        (feature, controller)
    }

    fn manual_triggered_feature_with_timeout_and_counter(
        trigger_timeout: Option<TriggerTimeout>,
    ) -> (
        DiscoveredLiveCaptureFeature,
        DeterministicFakeController,
        u64,
        Arc<AtomicUsize>,
    ) {
        let channels = (0..19)
            .map(|channel| {
                CaptureChannelId::new(format!("stream-bank-{}:{}", channel % 4, channel * 7 + 3))
            })
            .collect::<Vec<_>>();
        let mut trigger_conditions = vec![None; channels.len()];
        trigger_conditions[0] = Some(SimpleTriggerCondition::Rising);
        let config = DeterministicFakeConfig::new(channels.clone(), vec![3, 5, 2, 7], 0x5a17)
            .unwrap()
            .with_simple_trigger(trigger_conditions)
            .unwrap();
        let trigger_sample = config.first_trigger_sample().unwrap();
        let total_samples = config.total_samples();
        let (provider, controller) = DeterministicFakeProvider::manually_paced(config);
        let capabilities = streaming_capabilities(&channels);
        let prepare_calls = Arc::new(AtomicUsize::new(0));
        let feature = DiscoveredLiveCaptureFeature::new(
            NodeId(42),
            "Triggered Contract Fake",
            Box::new(FakeFeature {
                channel_names: (0..channels.len())
                    .map(|channel| format!("Streaming {channel}"))
                    .collect(),
                simple_trigger_channels: vec![SimpleTriggerChannel {
                    channel_id: channels[0].clone(),
                    viewer_channel: 0,
                    name: "Streaming 0".into(),
                    enabled: true,
                    condition: SimpleTriggerCondition::Rising,
                }],
                channels,
                sample_rate_hz: 1_000_000_000.0,
                prepare: Some(Box::new(move |context| provider.prepare(context))),
                prepare_calls: Arc::clone(&prepare_calls),
                capabilities,
                session_plan: Some(CaptureSessionPlan {
                    sample_rate_hz: 1_000_000_000,
                    channel_count: 19,
                    policy: EffectiveCapturePolicy {
                        requested: CapturePolicy {
                            start: RecordingStart::Trigger,
                            trigger_placement: None,
                            retention_before_origin: RetentionPolicy::Everything,
                            retention_after_origin: RetentionPolicy::Everything,
                            completion: CompletionPolicy::UntilStopped,
                            trigger_timeout,
                        },
                        effective: CapturePolicy {
                            start: RecordingStart::Trigger,
                            trigger_placement: None,
                            retention_before_origin: RetentionPolicy::Everything,
                            retention_after_origin: RetentionPolicy::Everything,
                            completion: CompletionPolicy::UntilStopped,
                            trigger_timeout,
                        },
                    },
                    capacity: CaptureCapacityEstimate {
                        worst_case_bytes_per_second: 2_375_000_000,
                        finite_capture_bytes: Some((total_samples * 19_u64).div_ceil(8)),
                        retained_duration: None,
                        sustainable: None,
                        warnings: Vec::new(),
                    },
                }),
            }),
        );
        (feature, controller, trigger_sample, prepare_calls)
    }

    fn manual_triggered_feature_with_counter() -> (
        DiscoveredLiveCaptureFeature,
        DeterministicFakeController,
        u64,
        Arc<AtomicUsize>,
    ) {
        manual_triggered_feature_with_timeout_and_counter(None)
    }

    fn manual_triggered_feature() -> (
        DiscoveredLiveCaptureFeature,
        DeterministicFakeController,
        u64,
    ) {
        let (feature, controller, trigger_sample, _) = manual_triggered_feature_with_counter();
        (feature, controller, trigger_sample)
    }

    fn buffered_triggered_feature() -> (
        DiscoveredLiveCaptureFeature,
        BufferedFakeController,
        u64,
        Arc<AtomicUsize>,
    ) {
        let channels = vec![
            CaptureChannelId::new("pod-a:3"),
            CaptureChannelId::new("pod-q:41"),
            CaptureChannelId::new("aux-bank:9"),
        ];
        let sample_rate_hz = 2_000_000_u64;
        let config = BufferedFakeConfig::new(
            channels.clone(),
            sample_rate_hz,
            19,
            5,
            0x8d31,
        )
        .unwrap()
        .with_simple_trigger(vec![None, Some(SimpleTriggerCondition::Falling), None])
        .unwrap();
        let trigger_sample = config.first_trigger_sample().unwrap();
        let capabilities = config.capabilities().clone();
        let (provider, controller) = BufferedFakeProvider::manually_uploaded(config);
        let prepare_calls = Arc::new(AtomicUsize::new(0));
        let feature = DiscoveredLiveCaptureFeature::new(
            NodeId(43),
            "Buffered Contract Fake",
            Box::new(FakeFeature {
                channel_names: vec!["Pod A 3".into(), "Pod Q 41".into(), "Aux 9".into()],
                simple_trigger_channels: vec![SimpleTriggerChannel {
                    channel_id: channels[1].clone(),
                    viewer_channel: 1,
                    name: "Pod Q 41".into(),
                    enabled: true,
                    condition: SimpleTriggerCondition::Falling,
                }],
                channels,
                sample_rate_hz: sample_rate_hz as f64,
                prepare: Some(Box::new(move |context| provider.prepare(context))),
                prepare_calls: Arc::clone(&prepare_calls),
                capabilities,
                session_plan: None,
            }),
        );
        (feature, controller, trigger_sample, prepare_calls)
    }

    fn poll_until(coordinator: &mut CaptureCoordinator, condition: impl Fn(&CaptureCoordinator) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition(coordinator) {
            assert!(Instant::now() < deadline, "capture coordinator timed out");
            coordinator.poll();
            std::thread::yield_now();
        }
    }

    fn run_triggered_coordinator_contract(
        feature: DiscoveredLiveCaptureFeature,
        expected_delivery: CaptureDataDelivery,
        expected_samples: u64,
        expected_trigger: u64,
        prepare_calls: Arc<AtomicUsize>,
        drive_capture: impl FnOnce(),
    ) {
        let source_node = feature.source_node;
        let channels = feature.channels().to_vec();
        assert_eq!(
            feature.capabilities().data_delivery(),
            expected_delivery
        );

        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        drive_capture();
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        let manifest = coordinator.completed_manifest().unwrap();
        assert_eq!(manifest.descriptor.channels(), channels);
        assert_eq!(manifest.committed_chunks, 4);
        assert_eq!(manifest.committed_samples, expected_samples);
        assert_eq!(coordinator.completed_recording_origin(), Some(expected_trigger));
        assert_eq!(coordinator.completed_trigger_sample(), Some(expected_trigger));
        assert_eq!(
            coordinator.status().unwrap().trigger_sample,
            Some(expected_trigger)
        );
        let states = coordinator.state_history();
        assert!(states.contains(&CaptureSessionState::Prepared));
        assert!(states.contains(&CaptureSessionState::Armed));
        assert!(states.contains(&CaptureSessionState::Triggered));
        assert!(states.contains(&CaptureSessionState::Recording));
        assert_eq!(states.last(), Some(&CaptureSessionState::Complete));

        let waveform = coordinator
            .take_waveform_update()
            .expect("coordinator should publish a waveform update")
            .expect("completed capture should retain its waveform");
        let mut viewer = logic_analyzer_viewer::LogicAnalyzerViewer::new();
        viewer.set_growing_capture(waveform);
        assert!(viewer.has_growing_capture());
        assert!(viewer.growing_capture_complete());

        let analysis = coordinator
            .take_analysis_attachment()
            .expect("coordinator should attach live analysis");
        assert_eq!(analysis.source_node, source_node);
        let analysis_schema = analysis
            .process
            .output_schema()
            .into_iter()
            .map(|port| (port.name, port.type_id, port.index, port.sample_kinds))
            .collect::<Vec<_>>();
        assert_eq!(analysis_schema.len(), channels.len());

        let first_replay = coordinator
            .create_replay_attachment()
            .unwrap()
            .expect("completed capture should be replayable");
        let second_replay = coordinator
            .create_replay_attachment()
            .unwrap()
            .expect("every replay should receive a fresh cursor");
        assert_eq!(first_replay.source_node, source_node);
        assert_eq!(second_replay.source_node, source_node);
        let replay_schema = |process: &dyn ProcessNode| {
            process
                .output_schema()
                .into_iter()
                .map(|port| (port.name, port.type_id, port.index, port.sample_kinds))
                .collect::<Vec<_>>()
        };
        assert_eq!(analysis_schema, replay_schema(first_replay.process.as_ref()));
        assert_eq!(analysis_schema, replay_schema(second_replay.process.as_ref()));
        assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn streaming_and_buffered_profiles_share_the_coordinator_contract() {
        let (feature, controller, trigger_sample, prepare_calls) =
            manual_triggered_feature_with_counter();
        run_triggered_coordinator_contract(
            feature,
            CaptureDataDelivery::DuringAcquisition,
            17,
            trigger_sample,
            prepare_calls,
            move || controller.grant_chunks(4),
        );

        let (feature, controller, trigger_sample, prepare_calls) =
            buffered_triggered_feature();
        run_triggered_coordinator_contract(
            feature,
            CaptureDataDelivery::BufferedUpload,
            19,
            trigger_sample,
            prepare_calls,
            move || {
                assert!(controller.wait_until_upload(Duration::from_secs(2)));
                controller.grant_upload_chunks(4);
            },
        );
    }

    #[test]
    fn development_registration_discovers_and_completes_a_capture() {
        let mut graph = NodeGraphWidget::new(nodes::build_registry());
        let source = graph
            .add_node_at(nodes::DemoCaptureSource::name(), egui::Pos2::ZERO)
            .unwrap();
        let feature = compiler::discover_live_capture_feature(
            graph.graph(),
            &compiler::BuilderRegistry::standard(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(feature.source_node, source);

        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        let manifest = coordinator.completed_manifest().unwrap();
        assert_eq!(manifest.committed_chunks, 64);
        assert_eq!(manifest.committed_samples, 64 * 4_096);
        assert_eq!(manifest.descriptor.channels().len(), 11);
    }

    #[test]
    fn immediate_capture_uses_commands_and_restores_editing_after_finalization() {
        let (feature, controller) = manual_feature();
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        assert!(!coordinator.graph_editing_enabled());

        controller.grant_chunks(2);
        poll_until(&mut coordinator, |coordinator| {
            coordinator
                .status()
                .is_some_and(|status| status.progress.captured_samples == Some(8))
        });
        coordinator.request_stop();
        coordinator.request_stop();
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        assert!(coordinator.graph_editing_enabled());
        assert_eq!(
            coordinator.state_history(),
            [
                CaptureSessionState::Preparing,
                CaptureSessionState::Prepared,
                CaptureSessionState::Recording,
                CaptureSessionState::Stopping,
                CaptureSessionState::Complete,
            ]
        );
        let manifest = coordinator.completed_manifest().unwrap();
        assert_eq!(manifest.descriptor.session_id(), coordinator.status().unwrap().session_id);
        assert_eq!(manifest.committed_chunks, 2);
        assert_eq!(manifest.committed_samples, 8);
    }

    #[test]
    fn force_trigger_uses_only_the_advertised_provider_operation() {
        let (feature, controller, _natural_trigger) = manual_triggered_feature();
        assert!(feature.capabilities().commands().force_trigger);
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        poll_until(&mut coordinator, |coordinator| {
            coordinator
                .status()
                .is_some_and(|status| status.state == CaptureSessionState::Armed)
        });

        coordinator.request_force_trigger().unwrap();
        poll_until(&mut coordinator, |coordinator| {
            coordinator
                .status()
                .is_some_and(|status| status.trigger_sample == Some(0))
        });
        controller.grant_chunks(4);
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        assert_eq!(coordinator.completed_recording_origin(), Some(0));
        assert_eq!(coordinator.completed_trigger_sample(), Some(0));
    }

    #[test]
    fn trigger_timeout_actions_continue_stop_and_force_through_capabilities() {
        let timeout = Duration::from_millis(20);

        let (feature, _controller, _, _) =
            manual_triggered_feature_with_timeout_and_counter(Some(TriggerTimeout {
                after: timeout,
                action: TriggerTimeoutAction::Stop,
            }));
        let mut stopped = CaptureCoordinator::new();
        stopped
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        poll_until(&mut stopped, |coordinator| !coordinator.is_active());
        assert_eq!(
            stopped.status().unwrap().completion,
            Some(signal_processing::CaptureCompletion::CancelledBeforeTrigger)
        );
        assert_eq!(stopped.completed_manifest().unwrap().committed_samples, 0);

        let (feature, controller, _, _) =
            manual_triggered_feature_with_timeout_and_counter(Some(TriggerTimeout {
                after: timeout,
                action: TriggerTimeoutAction::ForceTrigger,
            }));
        let mut forced = CaptureCoordinator::new();
        forced
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        poll_until(&mut forced, |coordinator| {
            coordinator
                .status()
                .is_some_and(|status| status.trigger_sample == Some(0))
        });
        controller.grant_chunks(4);
        poll_until(&mut forced, |coordinator| !coordinator.is_active());
        assert_eq!(forced.completed_recording_origin(), Some(0));

        let (feature, _controller, _, _) =
            manual_triggered_feature_with_timeout_and_counter(Some(TriggerTimeout {
                after: timeout,
                action: TriggerTimeoutAction::ContinueWaiting,
            }));
        let mut waiting = CaptureCoordinator::new();
        waiting
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        std::thread::sleep(timeout * 2);
        waiting.poll();
        assert!(waiting.is_active());
        assert_eq!(waiting.status().unwrap().state, CaptureSessionState::Armed);
        waiting.request_stop();
        poll_until(&mut waiting, |coordinator| !coordinator.is_active());
    }

    #[test]
    fn abort_retains_the_valid_committed_prefix_and_labels_it_incomplete() {
        let (feature, controller) = manual_feature();
        assert!(feature.capabilities().commands().abort);
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        controller.grant_chunks(1);
        poll_until(&mut coordinator, |coordinator| {
            coordinator
                .status()
                .is_some_and(|status| status.progress.captured_samples == Some(3))
        });

        coordinator.request_abort().unwrap();
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        let manifest = coordinator.completed_manifest().unwrap();
        assert_eq!(manifest.committed_chunks, 1);
        assert_eq!(manifest.committed_samples, 3);
        assert_eq!(
            coordinator.status().unwrap().completion,
            Some(signal_processing::CaptureCompletion::Aborted)
        );
    }

    #[test]
    fn health_reports_store_rate_summary_lag_and_graph_lag_without_blocking_capture() {
        let (feature, controller) = manual_feature();
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        poll_until(&mut coordinator, |coordinator| {
            coordinator
                .status()
                .is_some_and(|status| status.state == CaptureSessionState::Recording)
        });
        std::thread::sleep(Duration::from_millis(110));
        controller.grant_chunks(1);
        poll_until(&mut coordinator, |coordinator| {
            coordinator.status().is_some_and(|status| {
                status.health.input_bytes_per_second.is_some()
                    && status.health.summary_lag_samples.is_some()
            })
        });
        coordinator.set_graph_processed_samples(Some(1));

        let health = coordinator.status().unwrap().health;
        assert!(health.write_bytes_per_second.is_some());
        assert_eq!(health.retained_samples, Some(0));
        assert_eq!(health.graph_lag_samples, Some(2));

        coordinator.request_abort().unwrap();
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());
    }

    #[test]
    fn capture_now_bypasses_one_session_trigger_without_mutating_requested_policy() {
        let mut graph = NodeGraphWidget::new(nodes::build_registry());
        let source = graph
            .add_node_at(nodes::DemoCaptureSource::name(), egui::Pos2::ZERO)
            .unwrap();
        let mut state = serde_json::from_value::<nodes::DemoCaptureSourceState>(
            graph.graph().nodes[&source].state.clone(),
        )
        .unwrap();
        state
            .set_trigger_condition(0, SimpleTriggerCondition::Rising)
            .unwrap();
        graph.graph_mut().nodes.get_mut(&source).unwrap().state =
            serde_json::to_value(state).unwrap();
        let feature = compiler::discover_live_capture_feature(
            graph.graph(),
            &compiler::BuilderRegistry::standard(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            feature
                .session_plan()
                .unwrap()
                .policy
                .requested
                .start,
            signal_processing::RecordingStart::Trigger
        );

        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::CaptureNow)
            .unwrap();
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        assert_eq!(coordinator.completed_recording_origin(), Some(0));
        assert_eq!(coordinator.completed_trigger_sample(), None);
        let plan = coordinator.completed_session_plan().unwrap();
        assert_eq!(
            plan.policy.requested.start,
            signal_processing::RecordingStart::Trigger
        );
        assert_eq!(
            plan.policy.effective.start,
            signal_processing::RecordingStart::Immediate
        );
        assert_eq!(
            coordinator.completed_persisted_session_plan().as_ref(),
            Some(plan)
        );
    }

    #[test]
    fn preparation_failure_keeps_editing_locked_until_cleanup_returns() {
        let feature = DiscoveredLiveCaptureFeature::new(
            NodeId(99),
            "Failing Fake",
            Box::new(FailingFeature {
                channels: vec![CaptureChannelId::new("fake:0")],
                channel_names: vec!["Fake 0".into()],
                capabilities: CaptureProviderCapabilities::single(
                    CaptureDataDelivery::DuringAcquisition,
                    vec![CaptureChannelId::new("fake:0")],
                    1_000_000_000,
                ),
            }),
        );
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        assert!(!coordinator.graph_editing_enabled());

        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        assert!(coordinator.graph_editing_enabled());
        assert_eq!(
            coordinator.status().unwrap().state,
            CaptureSessionState::Error
        );
        assert!(
            coordinator
                .status()
                .unwrap()
                .error
                .as_deref()
                .is_some_and(|error| error.contains("intentional preparation failure"))
        );
        assert!(coordinator.completed_manifest().is_none());
    }

    #[test]
    fn triggered_capture_arms_marks_the_waveform_and_defines_recording_origin() {
        let (feature, controller, expected_trigger) = manual_triggered_feature();
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();

        controller.grant_chunks(4);
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        assert_eq!(coordinator.completed_recording_origin(), Some(expected_trigger));
        assert_eq!(coordinator.completed_trigger_sample(), Some(expected_trigger));
        assert_eq!(
            coordinator.status().unwrap().trigger_sample,
            Some(expected_trigger)
        );
        let states = coordinator.state_history();
        assert!(states.contains(&CaptureSessionState::Armed));
        assert!(states.contains(&CaptureSessionState::Triggered));
        assert!(states.contains(&CaptureSessionState::Recording));
        assert!(
            states.iter().position(|state| *state == CaptureSessionState::Armed)
                < states
                    .iter()
                    .position(|state| *state == CaptureSessionState::Triggered)
        );
    }

    #[test]
    fn paused_viewer_and_analysis_do_not_delay_manual_capture() {
        let (feature, controller) = manual_feature();
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let index = loop {
            coordinator.poll();
            if let Some(Some(index)) = coordinator.take_waveform_update()
            {
                break index;
            }
            assert!(Instant::now() < deadline, "waveform attachment timed out");
            std::thread::yield_now();
        };
        let mut viewer = logic_analyzer_viewer::LogicAnalyzerViewer::new();
        viewer.set_growing_capture(index);
        viewer.toggle_pause_display();
        let _paused_analysis = coordinator
            .take_analysis_attachment()
            .expect("analysis attachment should precede waveform publication");

        controller.grant_chunks(4);
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        assert!(viewer.display_paused());
        let manifest = coordinator.completed_manifest().unwrap();
        assert_eq!(manifest.committed_chunks, 4);
        assert_eq!(manifest.committed_samples, 17);
    }

    #[test]
    fn finalized_replay_creates_fresh_sources_without_preparing_provider_again() {
        let (feature, controller, prepare_calls) = manual_feature_with_counter();
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();

        controller.grant_chunks(4);
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());
        assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
        assert_eq!(coordinator.replay_source_node(), Some(NodeId(41)));

        let first = coordinator
            .create_replay_attachment()
            .unwrap()
            .expect("finalized session should be replayable");
        let second = coordinator
            .create_replay_attachment()
            .unwrap()
            .expect("every Run should get a fresh replay cursor");

        assert_eq!(first.source_node, NodeId(41));
        assert_eq!(second.source_node, NodeId(41));
        let schema = |process: &dyn ProcessNode| {
            process
                .output_schema()
                .into_iter()
                .map(|port| (port.name, port.type_id, port.index, port.sample_kinds))
                .collect::<Vec<_>>()
        };
        assert_eq!(schema(first.process.as_ref()), schema(second.process.as_ref()));
        assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    #[ignore = "requires a connected DSLogic U3Pro16 with runtime firmware and FPGA image"]
    fn u3pro16_buffered_hardware_capture_finalizes_and_opens_a_replay_source() {
        let mut graph = NodeGraphWidget::new(nodes::build_registry());
        let source = graph
            .add_node_at(nodes::DsLogicU3Pro16::name(), egui::Pos2::ZERO)
            .unwrap();
        let mut state = serde_json::from_value::<nodes::U3Pro16State>(
            graph.graph().nodes[&source].state.clone(),
        )
        .unwrap();
        state.mode.select("Buffer");
        state.sample_rate.select("1 MHz");
        state.duration_ms.value = 1;
        state.channels.enabled.fill(false);
        state.channels.enabled[0] = true;
        state.channels.enabled[1] = true;
        graph.graph_mut().nodes.get_mut(&source).unwrap().state =
            serde_json::to_value(state).unwrap();
        let feature = compiler::discover_live_capture_feature(
            graph.graph(),
            &compiler::BuilderRegistry::standard(),
        )
        .unwrap()
        .unwrap();
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        while coordinator.is_active() {
            assert!(Instant::now() < deadline, "hardware capture timed out");
            coordinator.poll();
            std::thread::yield_now();
        }

        let status = coordinator.status().unwrap();
        assert_eq!(status.state, CaptureSessionState::Complete, "{:?}", status.error);
        assert!(coordinator.completed_manifest().unwrap().committed_samples >= 1_024);
        let replay = coordinator
            .create_replay_attachment()
            .unwrap()
            .expect("finalized hardware capture should be replayable");
        assert_eq!(replay.source_node, source);
        assert_eq!(replay.process.output_schema().len(), 2);
    }

    #[test]
    #[ignore = "requires a connected SuperSpeed DSLogic U3Pro16 with runtime firmware and FPGA image"]
    fn u3pro16_streaming_hardware_capture_stops_at_the_host_limit_and_replays() {
        let mut graph = NodeGraphWidget::new(nodes::build_registry());
        let source = graph
            .add_node_at(nodes::DsLogicU3Pro16::name(), egui::Pos2::ZERO)
            .unwrap();
        let mut state = serde_json::from_value::<nodes::U3Pro16State>(
            graph.graph().nodes[&source].state.clone(),
        )
        .unwrap();
        state.mode.select("Stream");
        state.sample_rate.select("1 MHz");
        state.duration_ms.value = 10;
        state.channels.enabled.fill(false);
        state.channels.enabled[0] = true;
        state.channels.enabled[1] = true;
        graph.graph_mut().nodes.get_mut(&source).unwrap().state =
            serde_json::to_value(state).unwrap();
        let feature = compiler::discover_live_capture_feature(
            graph.graph(),
            &compiler::BuilderRegistry::standard(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            feature.capabilities().data_delivery(),
            signal_processing::CaptureDataDelivery::DuringAcquisition
        );
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        while coordinator.is_active() {
            assert!(Instant::now() < deadline, "hardware stream timed out");
            coordinator.poll();
            std::thread::yield_now();
        }

        let status = coordinator.status().unwrap();
        assert_eq!(status.state, CaptureSessionState::Complete, "{:?}", status.error);
        assert_eq!(coordinator.completed_manifest().unwrap().committed_samples, 10_000);
        let replay = coordinator
            .create_replay_attachment()
            .unwrap()
            .expect("finalized hardware stream should be replayable");
        assert_eq!(replay.source_node, source);
        assert_eq!(replay.process.output_schema().len(), 2);
    }
}
