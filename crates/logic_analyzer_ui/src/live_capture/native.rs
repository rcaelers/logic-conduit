use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use logic_analyzer_capture_export::{
    CaptureExportFormat as CaptureRawExportFormat, CaptureExportObserver, CaptureExportProgress,
    CaptureExportReport, export_finalized_capture,
};
use logic_analyzer_graph::host::DiscoveredLiveCaptureFeature;
use logic_analyzer_graph::node::CaptureGraphSourceFactory;
use signal_processing::{
    AcquisitionContext, CaptureAcquisitionPhase, CaptureCompletion, CaptureDataDelivery,
    CaptureEvent, CaptureEventPublishError, CaptureEventPublisher, CaptureEventQueueReader,
    CaptureHealth, CaptureIndex, CaptureMetadata, CaptureProgress, CaptureQueueReceiveError,
    CaptureRecordingGate, CaptureSampledWindow, CaptureSessionId, CaptureSessionOutcome,
    CaptureSessionPlan, CaptureSessionState, CaptureStartMode, CaptureStoreDescriptor,
    CaptureTimelineMetadata, NativeCaptureSessionPin, NativeCaptureSessionRepository,
    NativeCaptureSessionRepositoryConfig, NativeCaptureSessionSummary, NativeCaptureStore,
    NativeCaptureStoreConfig, NativeFinalizedCapture, NativeGrowingCaptureIndex,
    NativeGrowingCaptureIndexWorker, RecordingStart, TriggerTimeoutAction,
    bounded_capture_event_queue,
};

use super::implementation::{
    CaptureAnalysisAttachment, CaptureCoordinatorContract, CaptureExportCompletion,
    CaptureExportStatus, CaptureReplayAttachment, CaptureSessionStatus, CaptureWaveformUpdate,
};
use crate::app_platform::capture_session_directory;

const EVENT_QUEUE_CAPACITY: usize = 1_024;
const SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_millis(5);
const APPLICATION_METADATA_FILE: &str = "capture.application.json";
const APPLICATION_METADATA_TEMP_FILE: &str = "capture.application.json.tmp";
const APPLICATION_METADATA_OLD_FILE: &str = "capture.application.json.old";
const APPLICATION_METADATA_VERSION: u16 = 2;
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CaptureApplicationMetadata {
    format_version: u16,
    source_node: u32,
    source_title: String,
    sample_rate_hz: f64,
    channel_names: Vec<String>,
    graph: node_graph::GraphState,
    #[serde(default)]
    configuration_epochs: Vec<PersistedConfigurationEpoch>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedConfigurationEpoch {
    epoch_id: u64,
    source_sample: u64,
    analysis_sample: u64,
    timestamp_ns: u64,
    graph: node_graph::GraphState,
    outcome: PersistedConfigurationEpochOutcome,
    message: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PersistedConfigurationEpochOutcome {
    Pending,
    Applied,
    Deferred,
    Failed,
}

struct WorkerPreparedConfigurationEpoch {
    epoch_id: u64,
    source_sample: u64,
    boundary: signal_processing::ConfigurationBoundary,
}

enum CaptureCommand {
    Stop,
    Abort,
    ForceTrigger,
    PrepareConfigurationEpoch {
        graph: Box<node_graph::GraphState>,
        response: Sender<Result<WorkerPreparedConfigurationEpoch, String>>,
    },
    ResolveConfigurationEpoch {
        epoch_id: u64,
        resolution: super::implementation::ConfigurationEpochResolution,
        response: Sender<Result<(), String>>,
    },
}

struct CompletedCapture {
    _session_pin: NativeCaptureSessionPin,
    capture: NativeFinalizedCapture,
    waveform: NativeGrowingCaptureIndex,
    source_node: node_graph::NodeId,
    graph_source_factory: Arc<dyn CaptureGraphSourceFactory>,
    recording_origin: Option<u64>,
    session_plan: Option<CaptureSessionPlan>,
    outcome: CaptureSessionOutcome,
    completion: Option<CaptureCompletion>,
    waveform_worker: Option<NativeGrowingCaptureIndexWorker>,
}

struct PinnedCaptureIndex {
    inner: NativeGrowingCaptureIndex,
    _session_pin: NativeCaptureSessionPin,
}

impl CaptureIndex for PinnedCaptureIndex {
    fn display_name(&self) -> String {
        self.inner.display_name()
    }

    fn index_path(&self) -> &Path {
        self.inner.index_path()
    }

    fn header(&self) -> &CaptureMetadata {
        self.inner.header()
    }

    fn current_metadata(&self) -> CaptureMetadata {
        self.inner.current_metadata()
    }

    fn generation(&self) -> u64 {
        self.inner.generation()
    }

    fn is_complete(&self) -> bool {
        self.inner.is_complete()
    }

    fn capture_duration_us(&self) -> f64 {
        self.inner.capture_duration_us()
    }

    fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> signal_processing::Result<CaptureSampledWindow> {
        self.inner
            .sampled_window(channels, start_sample, end_sample, target_points)
    }
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
            let rate =
                u64::try_from((u128::from(bytes) * 1_000_000_000_u128) / elapsed.as_nanos().max(1))
                    .unwrap_or(u64::MAX);
            let snapshot = self.store.snapshot();
            let indexed = self.waveform.current_metadata().total_samples;
            let captured = progress.captured_samples.unwrap_or_else(|| {
                self.runtime
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .captured_samples
            });
            let _ = self.inner.publish(CaptureEvent::Health {
                session_id: self.store.descriptor().session_id(),
                health: CaptureHealth {
                    input_bytes_per_second: Some(rate),
                    write_bytes_per_second: Some(rate),
                    stored_samples: Some(snapshot.committed_samples),
                    summary_lag_samples: Some(captured.saturating_sub(indexed)),
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

struct PendingConfigurationEpoch {
    graph: node_graph::GraphState,
    response: Receiver<Result<WorkerPreparedConfigurationEpoch, String>>,
}

struct CaptureWorkerPorts {
    events: Box<dyn CaptureEventPublisher>,
    commands: Receiver<CaptureCommand>,
    waveform_ready: Sender<NativeGrowingCaptureIndex>,
    analysis_ready: Sender<CaptureAnalysisAttachment>,
}

struct CaptureWorkerSession {
    repository: NativeCaptureSessionRepository,
    application_metadata: Option<CaptureApplicationMetadata>,
}

struct ExportObserver {
    cancellation: Arc<AtomicBool>,
    progress: Sender<CaptureExportProgress>,
}

impl CaptureExportObserver for ExportObserver {
    fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Relaxed)
    }

    fn on_progress(&mut self, progress: CaptureExportProgress) {
        let _ = self.progress.try_send(progress);
    }
}

struct ActiveExport {
    cancellation: Arc<AtomicBool>,
    progress: Receiver<CaptureExportProgress>,
    completion: Receiver<Result<CaptureExportReport, String>>,
    worker: Option<JoinHandle<()>>,
}

pub(crate) struct CaptureCoordinator {
    repository: NativeCaptureSessionRepository,
    recent_sessions: Vec<NativeCaptureSessionSummary>,
    _ephemeral_root: Option<TempDir>,
    status: Option<CaptureSessionStatus>,
    active: Option<ActiveCapture>,
    completed: Option<CompletedCapture>,
    retired: Vec<CompletedCapture>,
    waveform_update: Option<CaptureWaveformUpdate>,
    analysis_attachment: Option<CaptureAnalysisAttachment>,
    export_status: Option<CaptureExportStatus>,
    export_notice: Option<Result<CaptureExportCompletion, String>>,
    active_export: Option<ActiveExport>,
    pending_configuration_epoch: Option<PendingConfigurationEpoch>,
    configuration_epoch_preparation:
        Option<Result<super::implementation::PreparedConfigurationEpoch, String>>,
    configuration_epoch_resolutions: Vec<Receiver<Result<(), String>>>,
    configuration_epoch_notice: Option<Result<(), String>>,
    state_history: Vec<CaptureSessionState>,
}

impl CaptureCoordinator {
    #[cfg(test)]
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary capture root must be available");
        let repository = NativeCaptureSessionRepository::new(
            NativeCaptureSessionRepositoryConfig::new(root.path()),
        )
        .expect("temporary capture repository must be available");
        Self::with_repository(repository, Some(root))
    }

    pub(crate) fn configured(max_recent_sessions: usize, max_total_bytes: u64) -> Self {
        let config = NativeCaptureSessionRepositoryConfig::new(capture_session_directory())
            .with_limits(max_recent_sessions, max_total_bytes)
            .expect("embedded live-capture limits are valid");
        let repository = NativeCaptureSessionRepository::new(config)
            .expect("the live-capture session directory must be available");
        Self::with_repository(repository, None)
    }

    fn with_repository(
        repository: NativeCaptureSessionRepository,
        ephemeral_root: Option<TempDir>,
    ) -> Self {
        let (recent_sessions, _) = repository.scan_with_cleanup_plan().unwrap_or_default();
        Self {
            repository,
            recent_sessions,
            _ephemeral_root: ephemeral_root,
            status: None,
            active: None,
            completed: None,
            retired: Vec::new(),
            waveform_update: None,
            analysis_attachment: None,
            export_status: None,
            export_notice: None,
            active_export: None,
            pending_configuration_epoch: None,
            configuration_epoch_preparation: None,
            configuration_epoch_resolutions: Vec::new(),
            configuration_epoch_notice: None,
            state_history: Vec::new(),
        }
    }

    pub(crate) fn current_session_id(&self) -> Option<CaptureSessionId> {
        self.completed
            .as_ref()
            .map(|completed| completed.capture.manifest().descriptor.session_id())
    }

    pub(crate) fn export_status(&self) -> Option<&CaptureExportStatus> {
        self.export_status.as_ref()
    }

    pub(crate) fn take_export_notice(&mut self) -> Option<Result<CaptureExportCompletion, String>> {
        self.export_notice.take()
    }

    pub(crate) fn start_export_current(
        &mut self,
        format: CaptureRawExportFormat,
        destination: PathBuf,
    ) -> Result<(), String> {
        if self.active_export.is_some() {
            return Err("a capture export is already active".into());
        }
        if self.is_active() {
            return Err("finish the live capture before exporting it".into());
        }
        let session_id = self
            .current_session_id()
            .ok_or_else(|| "there is no displayed capture to export".to_owned())?;
        let (capture, session_pin) = self
            .repository
            .open(session_id)
            .map_err(|error| format!("could not pin capture for export: {error}"))?;
        let total_samples = capture.manifest().committed_samples;
        let cancellation = Arc::new(AtomicBool::new(false));
        let (progress_sender, progress) = crossbeam_channel::bounded(1);
        let (completion_sender, completion) = crossbeam_channel::bounded(1);
        let worker_cancellation = Arc::clone(&cancellation);
        let worker_destination = destination.clone();
        let worker = std::thread::Builder::new()
            .name("capture-export".into())
            .spawn(move || {
                let _session_pin = session_pin;
                let mut observer = ExportObserver {
                    cancellation: worker_cancellation,
                    progress: progress_sender,
                };
                let result =
                    export_finalized_capture(&capture, format, &worker_destination, &mut observer);
                let _ = completion_sender.send(result);
            })
            .map_err(|error| format!("could not start capture export: {error}"))?;
        self.export_notice = None;
        self.export_status = Some(CaptureExportStatus {
            format_label: format.descriptor().label.to_owned(),
            destination,
            samples_written: 0,
            total_samples,
            cancelling: false,
        });
        self.active_export = Some(ActiveExport {
            cancellation,
            progress,
            completion,
            worker: Some(worker),
        });
        Ok(())
    }

    pub(crate) fn request_cancel_export(&mut self) {
        let Some(active) = &self.active_export else {
            return;
        };
        active.cancellation.store(true, Ordering::Relaxed);
        if let Some(status) = &mut self.export_status {
            status.cancelling = true;
        }
    }

    fn poll_export(&mut self) {
        let mut latest_progress = None;
        if let Some(active) = &self.active_export {
            while let Ok(progress) = active.progress.try_recv() {
                latest_progress = Some(progress);
            }
        }
        if let Some(progress) = latest_progress
            && let Some(status) = &mut self.export_status
        {
            status.samples_written = progress.samples_written;
            status.total_samples = progress.total_samples;
        }

        let completion =
            self.active_export
                .as_ref()
                .and_then(|active| match active.completion.try_recv() {
                    Ok(completion) => Some(completion),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        Some(Err("capture export worker stopped without a result".into()))
                    }
                });
        let Some(completion) = completion else {
            return;
        };
        if let Some(mut active) = self.active_export.take()
            && let Some(worker) = active.worker.take()
        {
            let _ = worker.join();
        }
        self.export_status = None;
        self.export_notice = Some(completion.map(|report| CaptureExportCompletion {
            destination: report.destination,
            warnings: report.warnings,
        }));
    }

    fn poll_configuration_epochs(&mut self) {
        let preparation =
            self.pending_configuration_epoch
                .as_ref()
                .and_then(|pending| match pending.response.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(Err(
                        "capture supervisor stopped while preparing a configuration epoch".into(),
                    )),
                });
        if let Some(preparation) = preparation {
            let pending = self
                .pending_configuration_epoch
                .take()
                .expect("preparation came from a pending epoch");
            self.configuration_epoch_preparation = Some(preparation.map(|prepared| {
                super::implementation::PreparedConfigurationEpoch {
                    epoch_id: prepared.epoch_id,
                    source_sample: prepared.source_sample,
                    boundary: prepared.boundary,
                    graph: pending.graph,
                }
            }));
        }

        let mut index = 0;
        while index < self.configuration_epoch_resolutions.len() {
            let result = match self.configuration_epoch_resolutions[index].try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(
                    "capture supervisor stopped before resolving a configuration epoch".into(),
                )),
            };
            if let Some(result) = result {
                self.configuration_epoch_resolutions.swap_remove(index);
                self.configuration_epoch_notice = Some(result);
            } else {
                index += 1;
            }
        }
    }

    fn pinned_waveform_update(
        &self,
        session_id: CaptureSessionId,
        waveform: NativeGrowingCaptureIndex,
    ) -> Result<CaptureWaveformUpdate, String> {
        let session_pin = self
            .repository
            .pin(session_id)
            .map_err(|error| format!("could not pin capture waveform: {error}"))?;
        Ok(Some(Box::new(PinnedCaptureIndex {
            inner: waveform,
            _session_pin: session_pin,
        })))
    }

    fn refresh_recent_sessions(&mut self) {
        if let Ok((sessions, _)) = self.repository.scan_with_cleanup_plan() {
            self.recent_sessions = sessions;
        }
    }

    pub(crate) fn start_with_graph(
        &mut self,
        feature: DiscoveredLiveCaptureFeature,
        graph: &node_graph::GraphState,
        mode: CaptureStartMode,
    ) -> Result<(), String> {
        self.start_session(feature, Some(graph), mode)
    }

    fn start_session(
        &mut self,
        feature: DiscoveredLiveCaptureFeature,
        graph: Option<&node_graph::GraphState>,
        mode: CaptureStartMode,
    ) -> Result<(), String> {
        if self.is_active() {
            return Err("a live capture is already active".into());
        }
        let commands = feature.capabilities().commands();
        if mode == CaptureStartMode::CaptureNow && !commands.capture_now {
            return Err("this capture source does not support Capture Now".into());
        }
        self.discard_all_capture_data()?;
        let session_id = fresh_session_id();
        let source_node = feature.source_node();
        let source_title = feature.source_title().to_owned();
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
            .unwrap_or_else(|| !feature.has_trigger_program())
            .then_some(0);
        let application_metadata = graph.map(|graph| CaptureApplicationMetadata {
            format_version: APPLICATION_METADATA_VERSION,
            source_node: source_node.0,
            source_title: source_title.clone(),
            sample_rate_hz: feature.sample_rate_hz(),
            channel_names: feature.channel_names().to_vec(),
            graph: graph.clone(),
            configuration_epochs: Vec::new(),
        });
        let repository = self.repository.clone();
        let (event_publisher, events) = bounded_capture_event_queue(EVENT_QUEUE_CAPACITY)
            .expect("capture event queue capacity is non-zero");
        let (command_sender, command_receiver) = crossbeam_channel::unbounded();
        let (completion_sender, completion_receiver) = crossbeam_channel::bounded(1);
        let (waveform_sender, waveform_receiver) = crossbeam_channel::bounded(1);
        let (analysis_sender, analysis_receiver) = crossbeam_channel::bounded(1);
        let worker = std::thread::Builder::new()
            .name("live-capture-supervisor".into())
            .spawn(move || {
                let completion = match run_capture_worker(
                    session_id,
                    feature,
                    mode,
                    CaptureWorkerSession {
                        repository,
                        application_metadata,
                    },
                    CaptureWorkerPorts {
                        events: Box::new(event_publisher),
                        commands: command_receiver,
                        waveform_ready: waveform_sender,
                        analysis_ready: analysis_sender,
                    },
                ) {
                    Ok(capture) => WorkerCompletion::Complete(Box::new(capture)),
                    Err(error) => WorkerCompletion::Failed(error),
                };
                let _ = completion_sender.send(completion);
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                return Err(format!("could not start capture supervisor: {error}"));
            }
        };

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
            outcome: CaptureSessionOutcome::InProgress,
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

    fn discard_all_capture_data(&mut self) -> Result<(), String> {
        if self.is_active() {
            return Err("cannot replace capture data while acquisition is active".into());
        }
        if self.active_export.is_some() {
            return Err("cannot replace capture data while it is being saved".into());
        }

        self.analysis_attachment = None;
        self.waveform_update = None;
        self.status = None;
        self.export_status = None;
        self.export_notice = None;

        let mut completed = self.completed.take().into_iter().collect::<Vec<_>>();
        completed.append(&mut self.retired);
        for capture in &mut completed {
            if let Some(worker) = capture.waveform_worker.take() {
                worker.join().map_err(|error| {
                    format!("could not finish the previous capture index: {error}")
                })?;
            }
        }
        drop(completed);

        self.refresh_recent_sessions();
        let session_ids = self
            .recent_sessions
            .iter()
            .filter_map(|session| session.session_id)
            .collect::<Vec<_>>();
        for session_id in session_ids {
            self.repository
                .discard(session_id)
                .map_err(|error| format!("could not remove previous capture data: {error}"))?;
        }
        self.refresh_recent_sessions();
        Ok(())
    }

    fn record_state(&mut self, state: CaptureSessionState) {
        if self.state_history.last().copied() != Some(state) {
            self.state_history.push(state);
        }
    }

    pub(crate) fn clear_completed(&mut self) {
        self.retire_completed();
        self.status = None;
        self.waveform_update = Some(None);
    }

    fn retire_completed(&mut self) {
        let Some(completed) = self.completed.take() else {
            return;
        };
        if completed
            .waveform_worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            self.retired.push(completed);
        }
    }

    fn replace_completed(&mut self, completed: CompletedCapture) {
        self.retire_completed();
        self.completed = Some(completed);
    }

    fn reap_waveform_workers(&mut self) {
        if let Some(completed) = &mut self.completed
            && completed
                .waveform_worker
                .as_ref()
                .is_some_and(NativeGrowingCaptureIndexWorker::is_finished)
            && let Some(worker) = completed.waveform_worker.take()
            && let Err(error) = worker.join()
            && let Some(status) = &mut self.status
        {
            status.error = Some(format!("could not rebuild capture waveform: {error}"));
        }
        let mut pending = Vec::new();
        for mut completed in self.retired.drain(..) {
            let finished = completed
                .waveform_worker
                .as_ref()
                .is_none_or(NativeGrowingCaptureIndexWorker::is_finished);
            if finished {
                if let Some(worker) = completed.waveform_worker.take() {
                    let _ = worker.join();
                }
            } else {
                pending.push(completed);
            }
        }
        self.retired = pending;
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
            CaptureEvent::Health { session_id, health } if session_id == status.session_id => {
                status.health = health;
            }
            CaptureEvent::Plan { session_id, plan } if session_id == status.session_id => {
                status.session_plan = Some(plan);
            }
            CaptureEvent::Triggered { session_id, sample } if session_id == status.session_id => {
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
                status.outcome = CaptureSessionOutcome::Incomplete;
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
                    status.outcome = capture.outcome;
                    status.completion = capture.completion;
                }
                self.record_state(CaptureSessionState::Complete);
                self.replace_completed(*capture);
            }
            WorkerCompletion::Failed(error) => {
                if let Some(status) = &mut self.status {
                    status.state = CaptureSessionState::Error;
                    status.phase = CaptureAcquisitionPhase::Finalizing;
                    status.outcome = CaptureSessionOutcome::Incomplete;
                    status.error = Some(error);
                }
                self.record_state(CaptureSessionState::Error);
                let previous = self.completed.as_ref().map(|completed| {
                    (
                        completed.capture.manifest().descriptor.session_id(),
                        completed.waveform.clone(),
                    )
                });
                if let Some((session_id, waveform)) = previous {
                    self.waveform_update =
                        Some(match self.pinned_waveform_update(session_id, waveform) {
                            Ok(update) => update,
                            Err(error) => {
                                if let Some(status) = &mut self.status {
                                    status.error = Some(error);
                                }
                                None
                            }
                        });
                }
            }
        }
        self.refresh_recent_sessions();
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
        self.poll_export();
        self.poll_configuration_epochs();
        self.reap_waveform_workers();
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
            let update = self.status.as_ref().map(|status| status.session_id).map_or(
                Err("capture status is unavailable for its waveform".to_owned()),
                |session_id| self.pinned_waveform_update(session_id, waveform),
            );
            match update {
                Ok(update) => self.waveform_update = Some(update),
                Err(error) => {
                    if let Some(status) = &mut self.status {
                        status.error = Some(error);
                    }
                }
            }
        }
        let mut hold_triggered_state = false;
        loop {
            let event = self.active.as_ref().map(|active| active.events.try_recv());
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

    fn request_configuration_epoch(&mut self, graph: node_graph::GraphState) -> Result<(), String> {
        if self.pending_configuration_epoch.is_some()
            || self.configuration_epoch_preparation.is_some()
        {
            return Err("a configuration epoch is already being prepared".into());
        }
        let status = self
            .status
            .as_ref()
            .ok_or_else(|| "capture status is unavailable".to_owned())?;
        if status.state != CaptureSessionState::Recording {
            return Err("configuration changes are accepted only while recording".into());
        }
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "there is no active capture".to_owned())?;
        let (response_sender, response) = crossbeam_channel::bounded(1);
        active
            .commands
            .send(CaptureCommand::PrepareConfigurationEpoch {
                graph: Box::new(graph.clone()),
                response: response_sender,
            })
            .map_err(|_| "capture supervisor no longer accepts configuration changes".to_owned())?;
        self.pending_configuration_epoch = Some(PendingConfigurationEpoch { graph, response });
        Ok(())
    }

    fn take_configuration_epoch_preparation(
        &mut self,
    ) -> Option<Result<super::implementation::PreparedConfigurationEpoch, String>> {
        self.configuration_epoch_preparation.take()
    }

    fn resolve_configuration_epoch(
        &mut self,
        epoch_id: u64,
        resolution: super::implementation::ConfigurationEpochResolution,
    ) -> Result<(), String> {
        let active = self.active.as_ref().ok_or_else(|| {
            "capture ended before the configuration epoch was resolved".to_owned()
        })?;
        let (response_sender, response) = crossbeam_channel::bounded(1);
        active
            .commands
            .send(CaptureCommand::ResolveConfigurationEpoch {
                epoch_id,
                resolution,
                response: response_sender,
            })
            .map_err(|_| "capture supervisor no longer accepts epoch outcomes".to_owned())?;
        self.configuration_epoch_resolutions.push(response);
        Ok(())
    }

    fn take_configuration_epoch_notice(&mut self) -> Option<Result<(), String>> {
        self.configuration_epoch_notice.take()
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
        let cursor =
            CaptureRecordingGate::finalized(completed.recording_origin).cursor(Box::new(cursor));
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

    fn graph_editing_enabled(&self) -> bool {
        !self.is_active()
            || self
                .status
                .as_ref()
                .is_some_and(|status| status.state == CaptureSessionState::Recording)
    }
}

impl Drop for CaptureCoordinator {
    fn drop(&mut self) {
        if let Some(mut export) = self.active_export.take() {
            export.cancellation.store(true, Ordering::Relaxed);
            if let Some(worker) = export.worker.take() {
                let _ = worker.join();
            }
        }
        if let Some(mut active) = self.active.take() {
            let _ = active.commands.try_send(CaptureCommand::Stop);
            drop(active.commands);
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

fn write_application_metadata(
    directory: &Path,
    metadata: &CaptureApplicationMetadata,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(metadata)
        .map_err(|error| format!("could not encode capture application metadata: {error}"))?;
    bytes.push(b'\n');
    let temporary = directory.join(APPLICATION_METADATA_TEMP_FILE);
    let final_path = directory.join(APPLICATION_METADATA_FILE);
    let old_path = directory.join(APPLICATION_METADATA_OLD_FILE);
    let _ = fs::remove_file(&temporary);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_data().map_err(|error| error.to_string())?;
    drop(file);
    let _ = fs::remove_file(&old_path);
    if final_path.exists() {
        fs::rename(&final_path, &old_path).map_err(|error| error.to_string())?;
    }
    if let Err(error) = fs::rename(&temporary, &final_path) {
        if old_path.exists() {
            let _ = fs::rename(&old_path, &final_path);
        }
        return Err(error.to_string());
    }
    let _ = fs::remove_file(old_path);
    Ok(())
}

#[cfg(test)]
fn read_application_metadata(directory: &Path) -> Result<CaptureApplicationMetadata, String> {
    let path = directory.join(APPLICATION_METADATA_FILE);
    recover_application_metadata_file(directory)?;
    let bytes =
        fs::read(&path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut metadata = serde_json::from_slice::<CaptureApplicationMetadata>(&bytes)
        .map_err(|error| format!("invalid {}: {error}", path.display()))?;
    if metadata.format_version != 1 && metadata.format_version != APPLICATION_METADATA_VERSION {
        return Err(format!(
            "unsupported capture application metadata version {}",
            metadata.format_version
        ));
    }
    let mut repaired = metadata.format_version != APPLICATION_METADATA_VERSION;
    metadata.format_version = APPLICATION_METADATA_VERSION;
    for epoch in &mut metadata.configuration_epochs {
        if epoch.outcome == PersistedConfigurationEpochOutcome::Pending {
            epoch.outcome = PersistedConfigurationEpochOutcome::Failed;
            epoch.message = Some("capture ended before this epoch outcome was recorded".into());
            repaired = true;
        }
    }
    if repaired {
        write_application_metadata(directory, &metadata)?;
    }
    Ok(metadata)
}

#[cfg(test)]
fn recover_application_metadata_file(directory: &Path) -> Result<(), String> {
    let final_path = directory.join(APPLICATION_METADATA_FILE);
    let temporary = directory.join(APPLICATION_METADATA_TEMP_FILE);
    let old_path = directory.join(APPLICATION_METADATA_OLD_FILE);
    if final_path.exists() {
        let _ = fs::remove_file(temporary);
        let _ = fs::remove_file(old_path);
        return Ok(());
    }
    if temporary.exists() {
        fs::rename(&temporary, &final_path).map_err(|error| error.to_string())?;
        let _ = fs::remove_file(old_path);
        return Ok(());
    }
    if old_path.exists() {
        fs::rename(old_path, final_path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn run_capture_worker(
    session_id: CaptureSessionId,
    feature: DiscoveredLiveCaptureFeature,
    mode: CaptureStartMode,
    session: CaptureWorkerSession,
    ports: CaptureWorkerPorts,
) -> Result<CompletedCapture, String> {
    let CaptureWorkerSession {
        repository,
        mut application_metadata,
    } = session;
    let session_pin = repository
        .reserve(session_id)
        .map_err(|error| error.to_string())?;
    if let Some(metadata) = &application_metadata
        && let Err(error) = write_application_metadata(session_pin.directory(), metadata)
    {
        drop(session_pin);
        let _ = repository.discard(session_id);
        return Err(error);
    }
    let CaptureWorkerPorts {
        events,
        commands,
        waveform_ready,
        analysis_ready,
    } = ports;
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
        .unwrap_or_else(|| feature.has_trigger_program());
    let host_enforces_completion =
        feature.capabilities().data_delivery() == CaptureDataDelivery::DuringAcquisition;
    let recording_gate = if triggered_recording {
        CaptureRecordingGate::pending()
    } else {
        CaptureRecordingGate::immediate()
    };
    let descriptor = CaptureStoreDescriptor::new(session_id, feature.channels().to_vec())
        .map_err(|error| error.to_string())?;
    let (store, writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
        session_pin.directory(),
        descriptor,
    ))
    .map_err(|error| error.to_string())?;
    let timeline =
        CaptureTimelineMetadata::new(feature.sample_rate_hz(), feature.channel_names().to_vec())
            .map_err(|error| error.to_string())?;
    store
        .write_timeline_metadata(timeline)
        .map_err(|error| error.to_string())?;
    let graph_source_factory = feature.graph_source_factory();
    let sample_rate_hz = feature.sample_rate_hz();
    let analysis_cursor = store.open_cursor().map_err(|error| error.to_string())?;
    let analysis_cursor = recording_gate.cursor(Box::new(analysis_cursor));
    let analysis_process = graph_source_factory
        .create(Box::new(analysis_cursor))
        .map_err(|error| format!("could not build live analysis source: {error}"))?;
    analysis_ready
        .send(CaptureAnalysisAttachment {
            source_node: feature.source_node(),
            process: analysis_process,
        })
        .map_err(|_| "live analysis attachment receiver closed".to_owned())?;
    let source_node = feature.source_node();
    let source_title = feature.source_title().to_owned();
    let (waveform, waveform_worker) = NativeGrowingCaptureIndex::spawn(
        store.clone(),
        source_title,
        feature.sample_rate_hz(),
        feature.channel_names().to_vec(),
    )
    .map_err(|error| error.to_string())?;
    let mut waveform_published = false;
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
            Ok(CaptureCommand::PrepareConfigurationEpoch { graph, response }) => {
                let result = prepare_configuration_epoch(
                    &mut application_metadata,
                    session_pin.directory(),
                    *graph,
                    &store,
                    &recording_gate,
                    sample_rate_hz,
                );
                let _ = response.send(result);
            }
            Ok(CaptureCommand::ResolveConfigurationEpoch {
                epoch_id,
                resolution,
                response,
            }) => {
                let result = resolve_configuration_epoch(
                    &mut application_metadata,
                    session_pin.directory(),
                    epoch_id,
                    resolution,
                );
                let _ = response.send(result);
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
        if host_enforces_completion
            && !stop_requested
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
        let waveform_metadata = waveform.current_metadata();
        if !waveform_published
            && waveform_ready_for_publication(
                triggered_recording,
                trigger_sample,
                waveform_metadata.total_samples,
                store.snapshot().committed_chunks != 0,
            )
        {
            let _ = waveform_ready.send(waveform.clone());
            waveform_published = true;
        }
    }
    let outcome = acquisition.join().map_err(|error| error.to_string())?;
    let resolution_deadline = Instant::now() + Duration::from_millis(500);
    while application_metadata.as_ref().is_some_and(|metadata| {
        metadata
            .configuration_epochs
            .iter()
            .any(|epoch| epoch.outcome == PersistedConfigurationEpochOutcome::Pending)
    }) && Instant::now() < resolution_deadline
    {
        match commands.recv_timeout(SUPERVISOR_POLL_INTERVAL) {
            Ok(CaptureCommand::ResolveConfigurationEpoch {
                epoch_id,
                resolution,
                response,
            }) => {
                let result = resolve_configuration_epoch(
                    &mut application_metadata,
                    session_pin.directory(),
                    epoch_id,
                    resolution,
                );
                let _ = response.send(result);
            }
            Ok(CaptureCommand::PrepareConfigurationEpoch { response, .. }) => {
                let _ = response.send(Err(
                    "capture ended before the configuration epoch was prepared".into(),
                ));
            }
            Ok(CaptureCommand::Stop | CaptureCommand::Abort | CaptureCommand::ForceTrigger) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    while let Ok(command) = commands.try_recv() {
        match command {
            CaptureCommand::ResolveConfigurationEpoch {
                epoch_id,
                resolution,
                response,
            } => {
                let result = resolve_configuration_epoch(
                    &mut application_metadata,
                    session_pin.directory(),
                    epoch_id,
                    resolution,
                );
                let _ = response.send(result);
            }
            CaptureCommand::PrepareConfigurationEpoch { response, .. } => {
                let _ = response.send(Err(
                    "capture ended before the configuration epoch was prepared".into(),
                ));
            }
            CaptureCommand::Stop | CaptureCommand::Abort | CaptureCommand::ForceTrigger => {}
        }
    }
    if !waveform_published {
        let _ = waveform_ready.send(waveform.clone());
    }
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
    let session_outcome = match outcome.completion {
        CaptureCompletion::Finished => CaptureSessionOutcome::Complete,
        CaptureCompletion::Stopped => CaptureSessionOutcome::Stopped,
        CaptureCompletion::CancelledBeforeTrigger => CaptureSessionOutcome::CancelledBeforeTrigger,
        CaptureCompletion::Aborted => CaptureSessionOutcome::Aborted,
    };
    let trigger_sample = runtime
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .trigger_sample;
    let capture = store
        .finalize_with_details(
            session_outcome,
            recording_gate.recording_origin(),
            trigger_sample,
        )
        .map_err(|error| error.to_string())?;
    Ok(CompletedCapture {
        _session_pin: session_pin,
        capture,
        waveform,
        source_node,
        graph_source_factory,
        recording_origin: recording_gate.recording_origin(),
        session_plan,
        outcome: session_outcome,
        completion: Some(outcome.completion),
        waveform_worker: None,
    })
}

fn waveform_ready_for_publication(
    triggered_recording: bool,
    trigger_sample: Option<u64>,
    indexed_samples: u64,
    has_written_chunks: bool,
) -> bool {
    if !has_written_chunks {
        return false;
    }
    if !triggered_recording {
        return true;
    }
    trigger_sample.is_some_and(|sample| indexed_samples > sample)
}

fn prepare_configuration_epoch(
    metadata: &mut Option<CaptureApplicationMetadata>,
    directory: &Path,
    graph: node_graph::GraphState,
    store: &NativeCaptureStore,
    recording_gate: &CaptureRecordingGate,
    sample_rate_hz: f64,
) -> Result<WorkerPreparedConfigurationEpoch, String> {
    let metadata = metadata
        .as_mut()
        .ok_or_else(|| "capture graph metadata is unavailable".to_owned())?;
    let recording_origin = recording_gate
        .recording_origin()
        .ok_or_else(|| "capture has not reached its recording origin".to_owned())?;
    let source_sample = store.snapshot().committed_samples.max(recording_origin);
    let analysis_sample = source_sample.saturating_sub(recording_origin);
    let timestamp_step_ns = (1_000_000_000.0 / sample_rate_hz).round();
    if !timestamp_step_ns.is_finite()
        || timestamp_step_ns <= 0.0
        || timestamp_step_ns > u64::MAX as f64
    {
        return Err(format!(
            "capture sample rate {sample_rate_hz} Hz cannot represent an epoch timestamp"
        ));
    }
    let timestamp_ns = source_sample.saturating_mul(timestamp_step_ns as u64);
    let epoch_id = metadata
        .configuration_epochs
        .last()
        .map_or(Ok(1), |epoch| epoch.epoch_id.checked_add(1).ok_or(()))
        .map_err(|()| "configuration epoch ID overflow".to_owned())?;
    metadata.graph = graph.clone();
    metadata
        .configuration_epochs
        .push(PersistedConfigurationEpoch {
            epoch_id,
            source_sample,
            analysis_sample,
            timestamp_ns,
            graph,
            outcome: PersistedConfigurationEpochOutcome::Pending,
            message: None,
        });
    write_application_metadata(directory, metadata)?;
    Ok(WorkerPreparedConfigurationEpoch {
        epoch_id,
        source_sample,
        boundary: signal_processing::ConfigurationBoundary::new(source_sample, timestamp_ns),
    })
}

fn resolve_configuration_epoch(
    metadata: &mut Option<CaptureApplicationMetadata>,
    directory: &Path,
    epoch_id: u64,
    resolution: super::implementation::ConfigurationEpochResolution,
) -> Result<(), String> {
    let metadata = metadata
        .as_mut()
        .ok_or_else(|| "capture graph metadata is unavailable".to_owned())?;
    let epoch = metadata
        .configuration_epochs
        .iter_mut()
        .find(|epoch| epoch.epoch_id == epoch_id)
        .ok_or_else(|| format!("configuration epoch {epoch_id} is missing"))?;
    if epoch.outcome != PersistedConfigurationEpochOutcome::Pending {
        return Err(format!(
            "configuration epoch {epoch_id} is already resolved"
        ));
    }
    let (outcome, message) = match resolution {
        super::implementation::ConfigurationEpochResolution::Applied => {
            (PersistedConfigurationEpochOutcome::Applied, None)
        }
        super::implementation::ConfigurationEpochResolution::Deferred(message) => {
            (PersistedConfigurationEpochOutcome::Deferred, Some(message))
        }
        super::implementation::ConfigurationEpochResolution::Failed(message) => {
            (PersistedConfigurationEpochOutcome::Failed, Some(message))
        }
    };
    epoch.outcome = outcome;
    epoch.message = message;
    write_application_metadata(directory, metadata)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use logic_analyzer_graph::host::{DiscoveredLiveCaptureFeature, GraphCompiler};
    use logic_analyzer_graph::{
        CaptureGraphSourceFactory, LiveCaptureEdit, LiveCaptureFeature, SimpleTriggerChannel,
    };
    use logic_analyzer_graph_nodes::test_support as nodes;
    use logic_analyzer_test_support::{
        BufferedFakeConfig, BufferedFakeController, BufferedFakeProvider, DeterministicFakeConfig,
        DeterministicFakeController, DeterministicFakeProvider,
    };
    use node_graph::{NodeGraphWidget, NodeId};
    use signal_processing::{
        AcquisitionContext, AcquisitionError, AcquisitionResult, CaptureAnalysisChannel,
        CaptureAnalysisSource, CaptureChannelId, CaptureCommandCapabilities, CaptureDataDelivery,
        CapturePolicy, CaptureProviderCapabilities, CaptureSessionPlan, CaptureSessionState,
        CaptureStartMode, CaptureStoreCursor, CompletionPolicy, EffectiveCapturePolicy,
        PreparedAcquisition, ProcessNode, RecordingStart, RetentionPolicy, SimpleTriggerCondition,
        TriggerTimeout, TriggerTimeoutAction,
    };

    use super::{
        ActiveCapture, CaptureCoordinator, CaptureCoordinatorContract, CaptureRawExportFormat,
        WorkerCompletion, bounded_capture_event_queue, waveform_ready_for_publication,
    };

    const TEST_LIVE_CAPTURE_SOURCE_ID: &str =
        "org.logicconduit.graph-node.test-live-capture-source/v1";
    const U3PRO16_ID: &str = "org.logicconduit.graph-node.dslogic-u3pro16/v1";

    fn registered_node_name(stable_id: &str) -> &'static str {
        nodes::registered_node_name(stable_id)
    }

    fn configure_u3pro16(
        state: &mut serde_json::Value,
        mode: &str,
        sample_rate: &str,
        duration_ms: u64,
        enabled_channels: &[usize],
    ) {
        state["mode"]["value"] = mode.into();
        state["sample_rate"]["value"] = sample_rate.into();
        state["duration"]["nanoseconds"] = duration_ms.saturating_mul(1_000_000).into();
        let channel_count = state["channels"]["enabled"]
            .as_array()
            .expect("U3Pro16 channels are an array")
            .len();
        let mut enabled = vec![false; channel_count];
        for &channel in enabled_channels {
            enabled[channel] = true;
        }
        state["channels"]["enabled"] = serde_json::to_value(enabled).unwrap();
    }

    impl CaptureCoordinator {
        fn start(
            &mut self,
            feature: DiscoveredLiveCaptureFeature,
            mode: CaptureStartMode,
        ) -> Result<(), String> {
            self.start_session(feature, None, mode)
        }
    }

    #[test]
    fn triggered_waveform_is_published_only_with_its_complete_trigger_prefix() {
        assert!(!waveform_ready_for_publication(true, None, 200, true));
        assert!(!waveform_ready_for_publication(true, Some(110), 110, true));
        assert!(waveform_ready_for_publication(true, Some(110), 111, true));
        assert!(!waveform_ready_for_publication(true, Some(0), 1, false));

        assert!(waveform_ready_for_publication(false, None, 0, true));
    }

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

    fn manual_feature_with_samples(
        sample_counts: Vec<u64>,
    ) -> (
        DiscoveredLiveCaptureFeature,
        DeterministicFakeController,
        Arc<AtomicUsize>,
    ) {
        let channels = vec![
            CaptureChannelId::new("bank-a:7"),
            CaptureChannelId::new("bank-c:2"),
        ];
        let config = DeterministicFakeConfig::new(channels.clone(), sample_counts, 0x5a17).unwrap();
        let (provider, controller) = DeterministicFakeProvider::manually_paced(config);
        let prepare_calls = Arc::new(AtomicUsize::new(0));
        let capabilities = streaming_capabilities(&channels);
        let feature = DiscoveredLiveCaptureFeature::new(
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

    fn manual_feature_with_counter() -> (
        DiscoveredLiveCaptureFeature,
        DeterministicFakeController,
        Arc<AtomicUsize>,
    ) {
        manual_feature_with_samples(vec![3, 5, 2, 7])
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
                    capture_window_samples: Some(total_samples),
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
        let config = BufferedFakeConfig::new(channels.clone(), sample_rate_hz, 19, 5, 0x8d31)
            .unwrap()
            .with_simple_trigger(vec![None, Some(SimpleTriggerCondition::Falling), None])
            .unwrap();
        let trigger_sample = config.first_trigger_sample().unwrap();
        let capabilities = config.capabilities().clone();
        let (provider, controller) = BufferedFakeProvider::manually_uploaded(config);
        let prepare_calls = Arc::new(AtomicUsize::new(0));
        let policy = CapturePolicy {
            start: RecordingStart::Trigger,
            trigger_placement: None,
            retention_before_origin: RetentionPolicy::Everything,
            retention_after_origin: RetentionPolicy::Everything,
            completion: CompletionPolicy::SamplesAfterOrigin(1),
            trigger_timeout: None,
        };
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
                session_plan: Some(CaptureSessionPlan {
                    sample_rate_hz,
                    channel_count: 3,
                    capture_window_samples: Some(21),
                    policy: EffectiveCapturePolicy {
                        requested: policy.clone(),
                        effective: policy,
                    },
                }),
            }),
        );
        (feature, controller, trigger_sample, prepare_calls)
    }

    fn poll_until(
        coordinator: &mut CaptureCoordinator,
        condition: impl Fn(&CaptureCoordinator) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition(coordinator) {
            assert!(Instant::now() < deadline, "capture coordinator timed out");
            coordinator.poll();
            std::thread::yield_now();
        }
    }

    #[test]
    fn failed_capture_does_not_detach_an_already_published_waveform() {
        let mut coordinator = CaptureCoordinator::new();
        let (commands, _command_receiver) = crossbeam_channel::unbounded();
        let (_completion_sender, completion) = crossbeam_channel::bounded(1);
        let (_waveform_sender, waveforms) = crossbeam_channel::bounded(1);
        let (_analysis_sender, analyses) = crossbeam_channel::bounded(1);
        let (_event_publisher, events) = bounded_capture_event_queue(1).unwrap();
        coordinator.active = Some(ActiveCapture {
            commands,
            completion,
            waveforms,
            analyses,
            events,
            worker: None,
            stop_requested: false,
            abort_requested: false,
        });

        coordinator.finish_worker(WorkerCompletion::Failed("stream overflow".into()));

        assert!(coordinator.take_waveform_update().is_none());
    }

    fn run_triggered_coordinator_contract(
        feature: DiscoveredLiveCaptureFeature,
        expected_delivery: CaptureDataDelivery,
        expected_samples: u64,
        expected_trigger: u64,
        prepare_calls: Arc<AtomicUsize>,
        drive_capture: impl FnOnce(),
    ) {
        let source_node = feature.source_node();
        let channels = feature.channels().to_vec();
        assert_eq!(feature.capabilities().data_delivery(), expected_delivery);

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
        assert_eq!(
            coordinator.completed_recording_origin(),
            Some(expected_trigger)
        );
        assert_eq!(
            coordinator.completed_trigger_sample(),
            Some(expected_trigger)
        );
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
        assert_eq!(
            analysis_schema,
            replay_schema(first_replay.process.as_ref())
        );
        assert_eq!(
            analysis_schema,
            replay_schema(second_replay.process.as_ref())
        );
        assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn finalized_capture_saves_as_pulseview_data_in_background() {
        let (feature, controller) = manual_feature();
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        controller.grant_chunks(4);
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        let output_dir = tempfile::tempdir().unwrap();
        let output = output_dir.path().join("background.sr");
        coordinator
            .start_export_current(CaptureRawExportFormat::Portable, output.clone())
            .unwrap();
        poll_until(&mut coordinator, |coordinator| {
            coordinator.export_status().is_none()
        });
        let completion = coordinator.take_export_notice().unwrap().unwrap();
        assert_eq!(completion.destination, output);
        assert!(completion.warnings.is_empty());

        assert!(std::fs::metadata(&completion.destination).unwrap().len() > 0);
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

        let (feature, controller, trigger_sample, prepare_calls) = buffered_triggered_feature();
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
            .add_node_at(
                registered_node_name(TEST_LIVE_CAPTURE_SOURCE_ID),
                egui::Pos2::ZERO,
            )
            .unwrap();
        let feature = GraphCompiler::new()
            .discover_live_capture_feature(graph.graph())
            .unwrap()
            .unwrap();
        assert_eq!(feature.source_node(), source);

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
    fn raw_only_capture_completes_after_its_analysis_attachment_is_dropped() {
        let mut graph = NodeGraphWidget::new(nodes::build_registry());
        graph
            .add_node_at(
                registered_node_name(TEST_LIVE_CAPTURE_SOURCE_ID),
                egui::Pos2::ZERO,
            )
            .unwrap();
        let feature = GraphCompiler::new()
            .discover_live_capture_feature(graph.graph())
            .unwrap()
            .unwrap();

        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            coordinator.poll();
            if coordinator.take_analysis_attachment().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "analysis attachment was not delivered"
            );
            std::thread::yield_now();
        }
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        assert_eq!(
            coordinator.status().unwrap().state,
            CaptureSessionState::Complete,
            "{:?}",
            coordinator.status().unwrap().error
        );
    }

    #[test]
    fn starting_a_new_capture_discards_the_previous_store_and_index() {
        let mut graph = NodeGraphWidget::new(nodes::build_registry());
        graph
            .add_node_at(
                registered_node_name(TEST_LIVE_CAPTURE_SOURCE_ID),
                egui::Pos2::ZERO,
            )
            .unwrap();
        let compiler = GraphCompiler::new();
        let feature = compiler
            .discover_live_capture_feature(graph.graph())
            .unwrap()
            .unwrap();
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start_with_graph(feature, graph.graph(), CaptureStartMode::SavedPolicy)
            .unwrap();
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());
        let first_session = coordinator.current_session_id().unwrap();
        let first_directory = coordinator
            .completed
            .as_ref()
            .unwrap()
            ._session_pin
            .directory()
            .to_owned();
        assert!(first_directory.exists());

        let feature = compiler
            .discover_live_capture_feature(graph.graph())
            .unwrap()
            .unwrap();
        coordinator
            .start_with_graph(feature, graph.graph(), CaptureStartMode::SavedPolicy)
            .unwrap();
        assert!(!first_directory.exists());
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());
        assert_ne!(coordinator.current_session_id(), Some(first_session));
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
        assert_eq!(
            manifest.descriptor.session_id(),
            coordinator.status().unwrap().session_id
        );
        assert_eq!(manifest.committed_chunks, 2);
        assert_eq!(manifest.committed_samples, 8);
    }

    #[test]
    fn configuration_epoch_is_persisted_before_runtime_application_and_resolved() {
        let (feature, controller) = manual_feature();
        let mut coordinator = CaptureCoordinator::new();
        let graph = node_graph::GraphState::default();
        coordinator
            .start_with_graph(feature, &graph, CaptureStartMode::SavedPolicy)
            .unwrap();
        poll_until(&mut coordinator, |coordinator| {
            coordinator
                .status()
                .is_some_and(|status| status.state == CaptureSessionState::Recording)
        });
        assert!(coordinator.graph_editing_enabled());
        controller.grant_chunks(2);
        poll_until(&mut coordinator, |coordinator| {
            coordinator
                .status()
                .and_then(|status| status.progress.captured_samples)
                == Some(8)
        });

        let mut edited = graph.clone();
        edited
            .set_extension("test.configuration_epoch", 1_u64)
            .unwrap();
        coordinator.request_configuration_epoch(edited).unwrap();
        let prepared = loop {
            coordinator.poll();
            if let Some(result) = coordinator.take_configuration_epoch_preparation() {
                break result.unwrap();
            }
            std::thread::yield_now();
        };
        assert_eq!(prepared.epoch_id, 1);
        // Provider progress can lead the batched durable-store frontier.
        // Epoch acceptance deliberately uses the latter.
        assert_eq!(prepared.source_sample, 0);
        assert_eq!(prepared.boundary.sample_index, 0);
        coordinator
            .resolve_configuration_epoch(
                prepared.epoch_id,
                super::super::implementation::ConfigurationEpochResolution::Applied,
            )
            .unwrap();
        loop {
            coordinator.poll();
            if let Some(result) = coordinator.take_configuration_epoch_notice() {
                result.unwrap();
                break;
            }
            std::thread::yield_now();
        }
        coordinator.request_stop();
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        let directory = coordinator
            .completed
            .as_ref()
            .unwrap()
            ._session_pin
            .directory();
        let metadata = super::read_application_metadata(directory).unwrap();
        assert_eq!(metadata.configuration_epochs.len(), 1);
        let epoch = &metadata.configuration_epochs[0];
        assert_eq!(epoch.source_sample, 0);
        assert_eq!(epoch.analysis_sample, 0);
        assert_eq!(epoch.timestamp_ns, 0);
        assert_eq!(
            epoch.outcome,
            super::PersistedConfigurationEpochOutcome::Applied
        );
    }

    #[test]
    fn interrupted_pending_configuration_epoch_recovers_as_failed() {
        let (feature, controller) = manual_feature();
        let mut coordinator = CaptureCoordinator::new();
        let graph = node_graph::GraphState::default();
        coordinator
            .start_with_graph(feature, &graph, CaptureStartMode::SavedPolicy)
            .unwrap();
        poll_until(&mut coordinator, |coordinator| {
            coordinator
                .status()
                .is_some_and(|status| status.state == CaptureSessionState::Recording)
        });
        controller.grant_chunks(1);
        let mut edited = graph.clone();
        edited
            .set_extension("test.configuration_epoch", 2_u64)
            .unwrap();
        coordinator.request_configuration_epoch(edited).unwrap();
        loop {
            coordinator.poll();
            if coordinator.take_configuration_epoch_preparation().is_some() {
                break;
            }
            std::thread::yield_now();
        }
        coordinator.request_stop();
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        let directory = coordinator
            .completed
            .as_ref()
            .unwrap()
            ._session_pin
            .directory();
        let metadata = super::read_application_metadata(directory).unwrap();
        let epoch = &metadata.configuration_epochs[0];
        assert_eq!(
            epoch.outcome,
            super::PersistedConfigurationEpochOutcome::Failed
        );
        assert!(epoch.message.as_deref().unwrap().contains("before"));
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
        poll_until(&mut waiting, |coordinator| {
            coordinator
                .status()
                .is_some_and(|status| status.state == CaptureSessionState::Armed)
        });
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
        assert_eq!(health.stored_samples, Some(0));
        assert_eq!(health.graph_lag_samples, Some(2));

        coordinator.request_abort().unwrap();
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());
    }

    #[test]
    fn capture_now_bypasses_one_session_trigger_without_mutating_requested_policy() {
        let mut graph = NodeGraphWidget::new(nodes::build_registry());
        let source = graph
            .add_node_at(
                registered_node_name(TEST_LIVE_CAPTURE_SOURCE_ID),
                egui::Pos2::ZERO,
            )
            .unwrap();
        let state = &graph.graph().nodes[&source].state;
        let edited = nodes::apply_registered_live_capture_edit(
            TEST_LIVE_CAPTURE_SOURCE_ID,
            state,
            &LiveCaptureEdit::SetSimpleTrigger {
                channel_id: CaptureChannelId::new("demo:0"),
                condition: SimpleTriggerCondition::Rising,
            },
        )
        .unwrap();
        graph.graph_mut().nodes.get_mut(&source).unwrap().state = edited;
        let feature = GraphCompiler::new()
            .discover_live_capture_feature(graph.graph())
            .unwrap()
            .unwrap();
        assert_eq!(
            feature.session_plan().unwrap().policy.requested.start,
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

        assert_eq!(
            coordinator.completed_recording_origin(),
            Some(expected_trigger)
        );
        assert_eq!(
            coordinator.completed_trigger_sample(),
            Some(expected_trigger)
        );
        assert_eq!(
            coordinator.status().unwrap().trigger_sample,
            Some(expected_trigger)
        );
        let states = coordinator.state_history();
        assert!(states.contains(&CaptureSessionState::Armed));
        assert!(states.contains(&CaptureSessionState::Triggered));
        assert!(states.contains(&CaptureSessionState::Recording));
        assert!(
            states
                .iter()
                .position(|state| *state == CaptureSessionState::Armed)
                < states
                    .iter()
                    .position(|state| *state == CaptureSessionState::Triggered)
        );
    }

    #[test]
    fn buffered_trigger_reveals_the_marker_with_the_indexed_pretrigger_prefix() {
        let (feature, controller, trigger_sample, _) = buffered_triggered_feature();
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();

        assert!(controller.wait_until_upload(Duration::from_secs(2)));
        poll_until(&mut coordinator, |coordinator| {
            coordinator
                .status()
                .is_some_and(|status| status.trigger_sample == Some(trigger_sample))
        });
        assert!(coordinator.take_waveform_update().is_none());

        let mut granted_chunks = 0;
        if trigger_sample >= 5 {
            controller.grant_upload_chunks(1);
            granted_chunks = 1;
            poll_until(&mut coordinator, |coordinator| {
                coordinator
                    .status()
                    .and_then(|status| status.progress.captured_samples)
                    .is_some_and(|samples| samples >= 5)
            });
            assert!(coordinator.take_waveform_update().is_none());
        }

        controller.grant_upload_chunks(4 - granted_chunks);
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());
        let waveform = coordinator
            .take_waveform_update()
            .expect("triggered capture should publish its waveform")
            .expect("completed triggered capture should retain its waveform");
        let metadata = waveform.current_metadata();
        assert_eq!(metadata.trigger_sample, Some(trigger_sample));
        assert!(metadata.total_samples > trigger_sample);
    }

    #[test]
    fn buffered_upload_is_not_cut_short_by_host_completion_policy() {
        let (feature, controller, trigger_sample, _) = buffered_triggered_feature();
        assert_eq!(trigger_sample, 10);
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();

        assert!(controller.wait_until_upload(Duration::from_secs(2)));
        controller.grant_upload_chunks(3);
        poll_until(&mut coordinator, |coordinator| {
            coordinator
                .status()
                .and_then(|status| status.progress.captured_samples)
                == Some(15)
        });

        let deadline = Instant::now() + Duration::from_millis(30);
        while Instant::now() < deadline {
            coordinator.poll();
            std::thread::yield_now();
        }
        assert!(
            coordinator.is_active(),
            "the host must not stop an upload for data already captured on the device"
        );

        controller.grant_upload_chunks(1);
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());
        assert_eq!(
            coordinator.completed_manifest().unwrap().committed_samples,
            19
        );
    }

    #[test]
    fn paused_viewer_and_analysis_do_not_delay_manual_capture() {
        let (feature, controller, _) = manual_feature_with_samples(vec![4; 32]);
        let mut coordinator = CaptureCoordinator::new();
        coordinator
            .start(feature, CaptureStartMode::SavedPolicy)
            .unwrap();

        controller.grant_chunks(16);
        let deadline = Instant::now() + Duration::from_secs(2);
        let index = loop {
            coordinator.poll();
            if let Some(Some(index)) = coordinator.take_waveform_update() {
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

        controller.grant_chunks(16);
        poll_until(&mut coordinator, |coordinator| !coordinator.is_active());

        assert!(viewer.display_paused());
        let manifest = coordinator.completed_manifest().unwrap();
        assert_eq!(manifest.committed_chunks, 32);
        assert_eq!(manifest.committed_samples, 128);
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
        assert_eq!(
            schema(first.process.as_ref()),
            schema(second.process.as_ref())
        );
        assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    #[ignore = "requires a connected DSLogic U3Pro16 with runtime firmware and FPGA image"]
    fn u3pro16_buffered_hardware_capture_finalizes_and_opens_a_replay_source() {
        let mut graph = NodeGraphWidget::new(nodes::build_registry());
        let source = graph
            .add_node_at(registered_node_name(U3PRO16_ID), egui::Pos2::ZERO)
            .unwrap();
        configure_u3pro16(
            &mut graph.graph_mut().nodes.get_mut(&source).unwrap().state,
            "Buffer",
            "1 MHz",
            1,
            &[0, 1],
        );
        let feature = GraphCompiler::new()
            .discover_live_capture_feature(graph.graph())
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
        assert_eq!(
            status.state,
            CaptureSessionState::Complete,
            "{:?}",
            status.error
        );
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
            .add_node_at(registered_node_name(U3PRO16_ID), egui::Pos2::ZERO)
            .unwrap();
        configure_u3pro16(
            &mut graph.graph_mut().nodes.get_mut(&source).unwrap().state,
            "Stream",
            "1 MHz",
            10,
            &[0, 1],
        );
        let feature = GraphCompiler::new()
            .discover_live_capture_feature(graph.graph())
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
        assert_eq!(
            status.state,
            CaptureSessionState::Complete,
            "{:?}",
            status.error
        );
        assert_eq!(
            coordinator.completed_manifest().unwrap().committed_samples,
            10_000
        );
        let replay = coordinator
            .create_replay_attachment()
            .unwrap()
            .expect("finalized hardware stream should be replayable");
        assert_eq!(replay.source_node, source);
        assert_eq!(replay.process.output_schema().len(), 2);
    }
}
