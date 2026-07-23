//! Complete plugin proof: typed image payload, source, collector, renderer, and panel.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use egui::{Color32, Rect, Stroke, StrokeKind};
use serde_json::Value;

use logic_analyzer_graph::{
    CompileCtx, DefaultViewerPayloadPresentation, PluginContext as GraphPluginContext, PortKind,
    PortValue, ResolvedInputs, RuntimeBuilder,
};
use logic_analyzer_ui::{
    PluginContext, PluginPanel, PluginPanelContext, PluginPanelDescriptor, PluginPanelIcon,
};
use logic_analyzer_viewer::{
    OpaqueLaneDrawContext, ViewerLaneBadge, ViewerLaneRenderer, ViewerLaneTrack,
};
use node_graph::{InputDef, NodeDef, OutputDef, Socket, SocketDef, SocketShape};
use signal_processing::{
    CollectedLaneIngestor, CollectedLaneQuery, CollectedLaneRequest, CollectedLaneSnapshotRequest,
    CollectedPayloadAdapter, DerivedDataRetention, InputPort, OpaqueCollectedLaneSnapshot,
    OutputPort, PortDirection, PortSchema, ProcessNode, WorkError, WorkResult,
};

const PAYLOAD_ID: &str = "org.logicconduit.example.camera-frame/v1";
const PANEL_ID: &str = "org.logicconduit.example.camera-panel/v1";
const IMAGE_SIZE: usize = 8;
const MAX_RETAINED_FRAMES: usize = 128;
const DRAIN_BATCH_SIZE: usize = 64;

#[derive(Clone, Debug)]
pub struct CameraFrame {
    pub timestamp_ns: u64,
    pub width: u16,
    pub height: u16,
    pub rgb: Arc<[u8]>,
}

impl PortValue for CameraFrame {
    fn kind_name() -> &'static str {
        "Camera Frame"
    }
}

pub struct CameraFrameSocket;

impl SocketDef for CameraFrameSocket {
    type Value = u64;

    fn type_name() -> &'static str {
        "Camera Frame"
    }

    fn color() -> Color32 {
        Color32::from_rgb(90, 175, 220)
    }

    fn shape() -> SocketShape {
        SocketShape::Diamond
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct CameraFrameSourceState;

pub struct CameraFrameSource;

impl NodeDef for CameraFrameSource {
    type State = CameraFrameSourceState;

    fn name() -> &'static str {
        "Camera Frame Source"
    }

    fn category() -> &'static str {
        "Plugin"
    }

    fn color() -> Color32 {
        Color32::from_rgb(45, 120, 155)
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        Vec::new()
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<CameraFrameSocket>("Frames")]
    }

    fn state() -> Self::State {
        CameraFrameSourceState
    }
}

struct CameraFrameSourceBuilder;

impl RuntimeBuilder for CameraFrameSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }

    fn derived_data_retention(&self, _state: &Value) -> DerivedDataRetention {
        DerivedDataRetention::Unlimited
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        Vec::new()
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<CameraFrame>()]
    }

    fn input_port(
        &self,
        _socket: &Socket,
        _member_index: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
        None
    }

    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        Some("frames".to_owned())
    }

    fn build(
        &self,
        name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(CameraFrameSourceNode::new(name)))
    }
}

struct CameraFrameSourceNode {
    name: String,
    next_frame: usize,
}

impl CameraFrameSourceNode {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            next_frame: 0,
        }
    }
}

impl ProcessNode for CameraFrameSourceNode {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        1
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        Vec::new()
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<CameraFrame>(
            "frames",
            0,
            PortDirection::Output,
        )]
    }

    fn work(&mut self, _inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        if self.next_frame >= 24 {
            return Err(WorkError::Shutdown);
        }
        let output = outputs
            .first()
            .and_then(|output| output.get::<CameraFrame>())
            .ok_or_else(|| WorkError::NodeError("missing camera-frame output".to_owned()))?;
        output.send(fake_frame(self.next_frame))?;
        self.next_frame += 1;
        Ok(1)
    }
}

fn fake_frame(index: usize) -> CameraFrame {
    let mut rgb = Vec::with_capacity(IMAGE_SIZE * IMAGE_SIZE * 3);
    for y in 0..IMAGE_SIZE {
        for x in 0..IMAGE_SIZE {
            rgb.extend_from_slice(&[
                ((x * 28 + index * 7) % 256) as u8,
                ((y * 30 + index * 11) % 256) as u8,
                (((x + y) * 17 + index * 13) % 256) as u8,
            ]);
        }
    }
    CameraFrame {
        timestamp_ns: index as u64 * 40_000_000,
        width: IMAGE_SIZE as u16,
        height: IMAGE_SIZE as u16,
        rgb: rgb.into(),
    }
}

#[derive(Clone)]
struct CameraFrameSnapshot {
    frames: Vec<CameraFrame>,
}

struct CameraFrameStorage {
    frames: RwLock<VecDeque<CameraFrame>>,
    generation: AtomicU64,
    live: AtomicBool,
}

impl CameraFrameStorage {
    fn new() -> Self {
        Self {
            frames: RwLock::new(VecDeque::new()),
            generation: AtomicU64::new(0),
            live: AtomicBool::new(true),
        }
    }
}

struct CameraFrameQuery {
    storage: Arc<CameraFrameStorage>,
}

impl CameraFrameQuery {
    fn latest(&self, max_items: usize) -> Vec<CameraFrame> {
        let frames = self.storage.frames.read().unwrap();
        frames
            .iter()
            .rev()
            .take(max_items)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

impl CollectedLaneQuery for CameraFrameQuery {
    fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }

    fn snapshot(
        &self,
        request: CollectedLaneSnapshotRequest,
    ) -> Option<OpaqueCollectedLaneSnapshot> {
        let frames = self
            .storage
            .frames
            .read()
            .unwrap()
            .iter()
            .filter(|frame| {
                frame.timestamp_ns >= request.start_time_ns
                    && frame.timestamp_ns <= request.end_time_ns
            })
            .take(request.max_items)
            .cloned()
            .collect();
        Some(OpaqueCollectedLaneSnapshot::new(Arc::new(
            CameraFrameSnapshot { frames },
        )))
    }

    fn nearest_time_boundary(&self, timestamp_ns: u64, max_distance_ns: u64) -> Option<u64> {
        self.storage
            .frames
            .read()
            .unwrap()
            .iter()
            .map(|frame| frame.timestamp_ns)
            .min_by_key(|candidate| candidate.abs_diff(timestamp_ns))
            .filter(|candidate| candidate.abs_diff(timestamp_ns) <= max_distance_ns)
    }

    fn timeline_extent_end_ns(&self) -> Option<u64> {
        self.storage
            .frames
            .read()
            .unwrap()
            .back()
            .map(|frame| frame.timestamp_ns)
    }

    fn is_live(&self) -> bool {
        self.storage.live.load(Ordering::Acquire)
    }
}

struct CameraFrameIngestor {
    storage: Arc<CameraFrameStorage>,
    buffer: VecDeque<CameraFrame>,
    finished: bool,
}

impl CameraFrameIngestor {
    fn new(request: CollectedLaneRequest) -> Self {
        let storage = Arc::new(CameraFrameStorage::new());
        request.publish_query(Arc::new(CameraFrameQuery {
            storage: Arc::clone(&storage),
        }));
        Self {
            storage,
            buffer: VecDeque::new(),
            finished: false,
        }
    }
}

impl CollectedLaneIngestor for CameraFrameIngestor {
    fn input_schema(&self, index: usize) -> PortSchema {
        PortSchema::new::<CameraFrame>(format!("in{index}"), index, PortDirection::Input)
    }

    fn drain(&mut self, input: &InputPort, retention: DerivedDataRetention) -> WorkResult<usize> {
        use crossbeam_channel::TryRecvError;

        let mut batch = Vec::new();
        if let Some(mut receiver) = input.get::<CameraFrame>(&mut self.buffer) {
            match receiver.try_recv_many(&mut batch, DRAIN_BATCH_SIZE) {
                Ok(_) | Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.finished = true,
            }
        } else {
            self.finished = true;
        }
        let count = batch.len();
        if count > 0 {
            let limit = match retention {
                DerivedDataRetention::Unlimited => MAX_RETAINED_FRAMES,
                DerivedDataRetention::MaxEntries(limit) => limit.clamp(1, MAX_RETAINED_FRAMES),
            };
            let mut frames = self.storage.frames.write().unwrap();
            frames.extend(batch);
            while frames.len() > limit {
                frames.pop_front();
            }
            self.storage.generation.fetch_add(1, Ordering::Release);
        }
        if self.finished {
            self.storage.live.store(false, Ordering::Release);
        }
        Ok(count)
    }

    fn is_finished(&self) -> bool {
        self.finished
    }
}

struct CameraFrameAdapter;

impl CollectedPayloadAdapter for CameraFrameAdapter {
    fn create_ingestor(
        &self,
        request: CollectedLaneRequest,
    ) -> Result<Box<dyn CollectedLaneIngestor>, String> {
        Ok(Box::new(CameraFrameIngestor::new(request)))
    }
}

struct CameraFrameRenderer;

impl ViewerLaneRenderer for CameraFrameRenderer {
    fn draw_opaque_lane(
        &self,
        _track: &ViewerLaneTrack,
        snapshot: Option<&OpaqueCollectedLaneSnapshot>,
        context: OpaqueLaneDrawContext<'_>,
    ) -> bool {
        let Some(snapshot) = snapshot.and_then(|snapshot| snapshot.value::<CameraFrameSnapshot>())
        else {
            return false;
        };
        for frame in &snapshot.frames {
            let x = context.time_to_x(frame.timestamp_ns);
            let size = context.height.clamp(12.0, 48.0);
            let rect = Rect::from_min_size(
                egui::pos2(x, context.top + 1.0),
                egui::vec2(size, context.height - 2.0),
            )
            .intersect(context.wave_rect);
            paint_thumbnail(context.painter, rect, frame);
        }
        true
    }
}

fn paint_thumbnail(painter: &egui::Painter, rect: Rect, frame: &CameraFrame) {
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    let width = usize::from(frame.width).max(1);
    let height = usize::from(frame.height).max(1);
    let cell_width = rect.width() / width as f32;
    let cell_height = rect.height() / height as f32;
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) * 3;
            let Some(rgb) = frame.rgb.get(index..index + 3) else {
                continue;
            };
            painter.rect_filled(
                Rect::from_min_max(
                    egui::pos2(
                        rect.left() + x as f32 * cell_width,
                        rect.top() + y as f32 * cell_height,
                    ),
                    egui::pos2(
                        rect.left() + (x + 1) as f32 * cell_width,
                        rect.top() + (y + 1) as f32 * cell_height,
                    ),
                ),
                0.0,
                Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
            );
        }
    }
    painter.rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0, Color32::WHITE),
        StrokeKind::Inside,
    );
}

#[derive(Default)]
struct CameraPanel {
    selected_from_end: usize,
}

impl PluginPanel for CameraPanel {
    fn show(&mut self, ui: &mut egui::Ui, context: PluginPanelContext<'_>) {
        let Some(query) = context
            .collected_lanes()
            .iter()
            .find(|lane| lane.payload().stable_id() == PAYLOAD_ID)
            .and_then(|lane| lane.query::<CameraFrameQuery>())
        else {
            ui.centered_and_justified(|ui| ui.weak("Run the Camera Frame Source to show frames"));
            return;
        };
        let frames = query.latest(32);
        if frames.is_empty() {
            ui.centered_and_justified(|ui| ui.weak("Waiting for camera frames"));
            return;
        }
        self.selected_from_end = self.selected_from_end.min(frames.len() - 1);
        ui.horizontal(|ui| {
            ui.label(format!("{} retained frame(s)", frames.len()));
            ui.add(
                egui::Slider::new(&mut self.selected_from_end, 0..=frames.len() - 1)
                    .text("History"),
            );
        });
        let frame = &frames[frames.len() - 1 - self.selected_from_end];
        ui.label(format!(
            "Timestamp: {:.3} ms",
            frame.timestamp_ns as f64 / 1e6
        ));
        let available = ui.available_rect_before_wrap();
        let side = available.width().min(available.height()).max(32.0);
        let rect = Rect::from_min_size(available.min, egui::vec2(side, side));
        ui.allocate_rect(rect, egui::Sense::hover());
        paint_thumbnail(ui.painter(), rect, frame);
    }

    fn save_state(&self) -> Value {
        serde_json::json!({
            "version": 1,
            "selected_from_end": self.selected_from_end,
        })
    }

    fn restore_state(&mut self, state: Value) -> Result<(), String> {
        let version = state.get("version").and_then(Value::as_u64).unwrap_or(1);
        if version != 1 {
            return Err(format!("unsupported camera-panel state version {version}"));
        }
        self.selected_from_end = state
            .get("selected_from_end")
            .and_then(Value::as_u64)
            .unwrap_or_default() as usize;
        Ok(())
    }
}

pub(crate) fn register_graph(ctx: &mut GraphPluginContext<'_>) -> Result<(), String> {
    ctx.register_node::<CameraFrameSource>()
        .register_builder("Camera Frame Source", Box::new(CameraFrameSourceBuilder))
        .register_collected_payload_subscription_adapter::<CameraFrame>(
            PAYLOAD_ID,
            Arc::new(CameraFrameAdapter),
            DefaultViewerPayloadPresentation::with_renderer(
                ViewerLaneBadge::new("IMG", Color32::from_rgb(90, 175, 220)),
                Arc::new(CameraFrameRenderer),
            ),
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn register_panel(ctx: &mut PluginContext<'_>) -> Result<(), String> {
    ctx.register_panel::<CameraPanel>(
        PluginPanelDescriptor::new(PANEL_ID, "Camera Frames")
            .icon(PluginPanelIcon::Image)
            .minimum_size(220.0, 180.0),
    )?;
    Ok(())
}

#[cfg(test)]
mod camera_frame_tests {
    use logic_analyzer_graph::{BuilderRegistry, CompileCtx, start_app_run};
    use node_graph::{NodeGraphWidget, SocketDirection, SocketId};

    use super::*;

    #[test]
    fn custom_source_collects_bounded_typed_frames_through_an_explicit_viewer() {
        let mut node_types = logic_analyzer_graph::nodes::build_registry();
        let mut builders = BuilderRegistry::standard();
        let mut plugins = GraphPluginContext::new(&mut node_types, &mut builders);
        register_graph(&mut plugins).unwrap();
        let mut widget = NodeGraphWidget::new(node_types);
        let source = widget
            .add_node_at("Camera Frame Source", egui::Pos2::ZERO)
            .unwrap();
        let viewer = widget
            .add_node_at("Viewer", egui::Pos2::new(200.0, 0.0))
            .unwrap();
        widget.graph_mut().add_connection(
            SocketId {
                node: source,
                index: 0,
                direction: SocketDirection::Output,
            },
            SocketId {
                node: viewer,
                index: 0,
                direction: SocketDirection::Input,
            },
        );
        let mut ctx = CompileCtx::default();
        let lanes = ctx.derived_lanes().clone();
        let mut run = start_app_run(widget.graph(), &builders, &mut ctx).unwrap();
        while !run.is_finished() {
            run.pump(64);
        }

        let lane = lanes
            .opaque_lanes()
            .into_iter()
            .find(|lane| lane.payload().stable_id() == PAYLOAD_ID)
            .unwrap();
        let query = lane.query::<CameraFrameQuery>().unwrap();
        let snapshot = lane
            .snapshot(CollectedLaneSnapshotRequest {
                start_time_ns: 0,
                end_time_ns: u64::MAX,
                max_items: 3,
            })
            .unwrap()
            .value::<CameraFrameSnapshot>()
            .unwrap();

        assert_eq!(query.latest(MAX_RETAINED_FRAMES).len(), 24);
        assert_eq!(snapshot.frames.len(), 3);
        assert_eq!(snapshot.frames[0].rgb.len(), IMAGE_SIZE * IMAGE_SIZE * 3);
    }
}
