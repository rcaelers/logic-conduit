use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use tempfile::TempDir;

use logic_analyzer_graph::compiler::{CaptureGraphSourceFactory, DiscoveredLiveCaptureFeature};
use logic_analyzer_processing::AcquisitionContext;
use signal_processing::{
    CaptureAcquisitionPhase, CaptureEvent, CaptureEventQueueReader, CaptureProgress,
    CaptureQueueReceiveError, CaptureSessionId, CaptureSessionState, CaptureStoreDescriptor,
    NativeCaptureStore, NativeCaptureStoreConfig, NativeFinalizedCapture,
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
}

struct CompletedCapture {
    _directory: TempDir,
    capture: NativeFinalizedCapture,
    waveform: NativeGrowingCaptureIndex,
    source_node: node_graph::NodeId,
    graph_source_factory: Arc<dyn CaptureGraphSourceFactory>,
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

    fn apply_event(&mut self, event: CaptureEvent) {
        let Some(status) = &mut self.status else {
            return;
        };
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
                    return;
                }
                status.state = event.state;
                status.phase = event.phase;
                self.record_state(event.state);
            }
            CaptureEvent::Progress {
                session_id,
                progress,
            } if session_id == status.session_id => status.progress = progress,
            CaptureEvent::Failed(failure) if failure.session_id == status.session_id => {
                status.state = CaptureSessionState::Error;
                status.phase = CaptureAcquisitionPhase::Finalizing;
                status.error = Some(failure.message);
                self.record_state(CaptureSessionState::Error);
            }
            _ => {}
        }
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

    fn start(&mut self, feature: DiscoveredLiveCaptureFeature) -> Result<(), String> {
        if self.is_active() {
            return Err("a live capture is already active".into());
        }
        let session_id = fresh_session_id();
        let source_node = feature.source_node;
        let source_title = feature.source_title.clone();
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
        });
        Ok(())
    }

    fn request_stop(&mut self) {
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
        loop {
            let event = self
                .active
                .as_ref()
                .map(|active| active.events.try_recv());
            match event {
                Some(Ok(event)) => self.apply_event(event),
                Some(Err(CaptureQueueReceiveError::Empty | CaptureQueueReceiveError::Closed))
                | None => break,
                Some(Err(CaptureQueueReceiveError::Timeout)) => unreachable!(),
            }
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
) -> Result<CompletedCapture, String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let descriptor = CaptureStoreDescriptor::new(session_id, feature.channels().to_vec())
        .map_err(|error| error.to_string())?;
    let (store, writer) =
        NativeCaptureStore::create(NativeCaptureStoreConfig::new(directory.path(), descriptor))
            .map_err(|error| error.to_string())?;
    let graph_source_factory = feature.graph_source_factory();
    let analysis_cursor = store.open_cursor().map_err(|error| error.to_string())?;
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
    let context = AcquisitionContext::new(session_id, Box::new(writer), events);
    let mut acquisition = feature
        .prepare(context)
        .map_err(|error| error.to_string())?;
    acquisition.start().map_err(|error| error.to_string())?;

    let mut stop_requested = false;
    while !acquisition.is_finished() {
        match commands.recv_timeout(SUPERVISOR_POLL_INTERVAL) {
            Ok(CaptureCommand::Stop) if !stop_requested => {
                stop_requested = true;
                acquisition
                    .request_stop()
                    .map_err(|error| error.to_string())?;
            }
            Ok(CaptureCommand::Stop) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) if !stop_requested => {
                stop_requested = true;
                acquisition
                    .request_stop()
                    .map_err(|error| error.to_string())?;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {}
        }
    }
    acquisition.join().map_err(|error| error.to_string())?;
    waveform_worker.join().map_err(|error| error.to_string())?;
    let capture = store.finalize().map_err(|error| error.to_string())?;
    Ok(CompletedCapture {
        _directory: directory,
        capture,
        waveform,
        source_node,
        graph_source_factory,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use logic_analyzer_graph::compiler::{
        CaptureGraphSourceFactory, DiscoveredLiveCaptureFeature, LiveCaptureFeature,
    };
    use logic_analyzer_graph::{compiler, nodes};
    use logic_analyzer_processing::{
        AcquisitionContext, AcquisitionError, AcquisitionResult, CaptureAnalysisChannel,
        CaptureAnalysisSource, DeterministicFakeConfig, DeterministicFakeController,
        DeterministicFakeProvider, PreparedAcquisition,
    };
    use node_graph::{NodeDef, NodeGraphWidget, NodeId};
    use signal_processing::{
        CaptureChannelId, CaptureSessionState, CaptureStoreCursor, ProcessNode,
    };

    use super::{CaptureCoordinator, CaptureCoordinatorContract};

    struct FakeFeature {
        channels: Vec<CaptureChannelId>,
        channel_names: Vec<String>,
        provider: DeterministicFakeProvider,
        prepare_calls: Arc<AtomicUsize>,
    }

    struct TestGraphSourceFactory {
        channels: Vec<CaptureChannelId>,
    }

    impl CaptureGraphSourceFactory for TestGraphSourceFactory {
        fn create(
            &self,
            cursor: Box<dyn CaptureStoreCursor>,
        ) -> Result<Box<dyn ProcessNode>, String> {
            test_analysis_source(&self.channels, cursor)
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
            1_000_000_000.0
        }

        fn graph_source_factory(&self) -> Arc<dyn CaptureGraphSourceFactory> {
            Arc::new(TestGraphSourceFactory {
                channels: self.channels.clone(),
            })
        }

        fn prepare(
            self: Box<Self>,
            context: AcquisitionContext,
        ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
            self.prepare_calls.fetch_add(1, Ordering::SeqCst);
            self.provider.prepare(context)
        }
    }

    struct FailingFeature {
        channels: Vec<CaptureChannelId>,
        channel_names: Vec<String>,
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

        fn graph_source_factory(&self) -> Arc<dyn CaptureGraphSourceFactory> {
            Arc::new(TestGraphSourceFactory {
                channels: self.channels.clone(),
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
        CaptureAnalysisSource::new("test-live-analysis", cursor, 1_000_000_000.0, layout)
            .map(|source| Box::new(source) as Box<dyn ProcessNode>)
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
        let feature =
            DiscoveredLiveCaptureFeature::new(
                NodeId(41),
                "Contract Fake",
                Box::new(FakeFeature {
                    channel_names: vec!["Bank A 7".into(), "Bank C 2".into()],
                    channels,
                    provider,
                    prepare_calls: Arc::clone(&prepare_calls),
                }),
            );
        (feature, controller, prepare_calls)
    }

    fn manual_feature() -> (DiscoveredLiveCaptureFeature, DeterministicFakeController) {
        let (feature, controller, _) = manual_feature_with_counter();
        (feature, controller)
    }

    fn poll_until(coordinator: &mut CaptureCoordinator, condition: impl Fn(&CaptureCoordinator) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition(coordinator) {
            assert!(Instant::now() < deadline, "capture coordinator timed out");
            coordinator.poll();
            std::thread::yield_now();
        }
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
        coordinator.start(feature).unwrap();
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
        coordinator.start(feature).unwrap();
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
    fn preparation_failure_keeps_editing_locked_until_cleanup_returns() {
        let feature = DiscoveredLiveCaptureFeature::new(
            NodeId(99),
            "Failing Fake",
            Box::new(FailingFeature {
                channels: vec![CaptureChannelId::new("fake:0")],
                channel_names: vec!["Fake 0".into()],
            }),
        );
        let mut coordinator = CaptureCoordinator::new();
        coordinator.start(feature).unwrap();
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
    fn paused_viewer_and_analysis_do_not_delay_manual_capture() {
        let (feature, controller) = manual_feature();
        let mut coordinator = CaptureCoordinator::new();
        coordinator.start(feature).unwrap();

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
        coordinator.start(feature).unwrap();

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
}
