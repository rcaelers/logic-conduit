use crate::logic_analyzer_viewer::LogicAnalyzerViewer;
use egui::Color32;
use node_graph::{
    EnumValue, FileValue, FloatValue, InputDef, IntValue, NodeDef, NodeGraphWidget,
    NodeTypeRegistry, OutputDef, PropDef, Socket, SocketDef, SocketDirection, SocketId,
    SocketShape, StringValue,
};
use serde::{Deserialize, Serialize};

pub struct App {
    node_graph: NodeGraphWidget,
    logic_analyzer: LogicAnalyzerViewer,
    analyzer_split: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        install_fonts(&cc.egui_ctx);
        let registry = build_registry();
        let mut widget = NodeGraphWidget::new(registry);
        populate_demo(&mut widget);
        Self {
            node_graph: widget,
            logic_analyzer: LogicAnalyzerViewer::demo(),
            analyzer_split: 0.42,
        }
    }
}

/// Adds Noto Sans Symbols 2 as a fallback font: the single, consistent
/// source for menu icon glyphs (modifier keys, undo/redo, cut/copy/paste/
/// duplicate) that egui's bundled fonts don't cover.
fn install_fonts(ctx: &egui::Context) {
    const FONT_NAME: &str = "noto-sans-symbols-2";
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        FONT_NAME.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/NotoSansSymbols2-Regular.ttf"
        ))),
    );
    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap()
        .push(FONT_NAME.to_owned());
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod font_tests {
    use super::install_fonts;

    #[test]
    fn menu_icon_glyphs_are_available() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        // `set_fonts` only takes effect at the start of the *next* pass.
        ctx.begin_pass(Default::default());
        let _ = ctx.end_pass();
        ctx.begin_pass(Default::default());
        let font_id = egui::FontId::proportional(14.0);
        ctx.fonts_mut(|fonts| {
            // Every glyph sourced from our bundled Noto Sans Symbols 2 font
            // (as opposed to egui's own bundled fonts, which already cover
            // e.g. ✂ 🗐 ▣ and aren't ours to regression-test here).
            for c in ['⇧', '⌘', '⌥', '⇪', '⏎', '⭮', '⭯', '🗎'] {
                assert!(
                    fonts.has_glyph(&font_id, c),
                    "missing glyph for {c:?} (U+{:04X})",
                    c as u32
                );
            }
        });
        let _ = ctx.end_pass();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let available = ui.available_size();
        let splitter_hit_height = 7.0;
        let splitter_visual_height = 2.0;
        let usable_height = (available.y - splitter_hit_height).max(0.0);
        let analyzer_min = 160.0;
        let graph_min = 160.0;
        let mut analyzer_height = usable_height * self.analyzer_split;
        if usable_height >= analyzer_min + graph_min {
            analyzer_height = analyzer_height.clamp(analyzer_min, usable_height - graph_min);
        }

        if let Some(file) = self.dsl_file_source_path() {
            self.logic_analyzer.set_capture_path(file);
        }

        let origin = ui.cursor().min;
        let splitter_rect = egui::Rect::from_min_size(
            egui::pos2(origin.x, origin.y + analyzer_height),
            egui::vec2(available.x, splitter_hit_height),
        );
        let splitter_id = ui.id().with("logic_analyzer_node_graph_splitter");
        let splitter_response =
            ui.interact(splitter_rect, splitter_id, egui::Sense::click_and_drag());
        if splitter_response.hovered() || splitter_response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if splitter_response.dragged() && usable_height > 0.0 {
            analyzer_height = (splitter_response
                .interact_pointer_pos()
                .map_or(analyzer_height, |pos| {
                    pos.y - origin.y - splitter_hit_height * 0.5
                }))
            .clamp(0.0, usable_height);
            if usable_height >= analyzer_min + graph_min {
                analyzer_height = analyzer_height.clamp(analyzer_min, usable_height - graph_min);
            }
            self.analyzer_split = (analyzer_height / usable_height).clamp(0.05, 0.95);
        }
        let graph_height = (usable_height - analyzer_height).max(0.0);

        ui.allocate_ui(egui::vec2(available.x, analyzer_height), |ui| {
            self.logic_analyzer.show(ui);
        });

        ui.allocate_space(egui::vec2(available.x, splitter_hit_height));
        let splitter_color = if splitter_response.dragged() || splitter_response.hovered() {
            egui::Color32::from_rgb(90, 90, 90)
        } else {
            egui::Color32::from_rgb(58, 58, 58)
        };
        let visual_rect = egui::Rect::from_center_size(
            splitter_rect.center(),
            egui::vec2(splitter_rect.width(), splitter_visual_height),
        );
        ui.painter().rect_filled(visual_rect, 0.0, splitter_color);

        ui.allocate_ui(egui::vec2(available.x, graph_height), |ui| {
            self.node_graph.show(ui);
        });
    }
}

impl App {
    fn dsl_file_source_path(&self) -> Option<String> {
        self.node_graph
            .graph()
            .nodes
            .values()
            .filter(|node| node.title == DslFileSource::name())
            .filter_map(|node| {
                serde_json::from_value::<DslFileSourceState>(node.state.clone()).ok()
            })
            .map(|state| state.file.value)
            .find(|path| !path.is_empty())
    }
}

// ── Custom socket types ───────────────────────────────────────────────────────

struct Signal;
impl SocketDef for Signal {
    type Value = bool;

    fn type_name() -> &'static str {
        "Signal"
    }
    fn color() -> Color32 {
        Color32::from_rgb(95, 155, 95)
    }
}

struct Protocol;
impl SocketDef for Protocol {
    type Value = bool;

    fn type_name() -> &'static str {
        "Protocol"
    }
    fn color() -> Color32 {
        Color32::from_rgb(200, 140, 60)
    }
    fn shape() -> SocketShape {
        SocketShape::Diamond
    }
}

// ── Node state ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DslFileSourceState {
    file: FileValue,
    channels: IntValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsbLogicAnalyzerState {
    sample_rate: IntValue,
    channels: IntValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpiDecoderState {
    mode: EnumValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UartDecoderState {
    baud_rate: IntValue,
    parity: EnumValue,
    stop_bits: EnumValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParallelDecoderState {
    width: IntValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProtocolFilterState {
    pattern: StringValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TimeWindowState {
    start_ms: FloatValue,
    end_ms: FloatValue,
}

// ── Node type definitions ─────────────────────────────────────────────────────

struct DslFileSource;
impl NodeDef for DslFileSource {
    type State = DslFileSourceState;

    fn name() -> &'static str {
        "DSL File Source"
    }
    fn category() -> &'static str {
        "Sources"
    }
    fn color() -> Color32 {
        Color32::from_rgb(100, 75, 140)
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::control::<node_graph::FileSocket>("File", |state| &mut state.file),
            InputDef::control::<node_graph::IntSocket>("Channels", |state| &mut state.channels),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        (0..32_usize)
            .map(|i| OutputDef::new::<Signal>(format!("Ch {i}")))
            .collect()
    }

    fn state() -> Self::State {
        DslFileSourceState {
            file: FileValue::with_filter(
                "_captures/wipneus5.dsl",
                "Select DSLogic capture",
                "DSLogic captures",
                &["dsl"],
            ),
            channels: IntValue::new(11, 1, 32),
        }
    }

    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        let channels = (state.channels.value as usize).clamp(1, 32);
        for (index, output) in outputs.iter_mut().enumerate() {
            output.visible = index < channels;
        }
    }
}

struct UsbLogicAnalyzer;
impl NodeDef for UsbLogicAnalyzer {
    type State = UsbLogicAnalyzerState;

    fn name() -> &'static str {
        "USB Logic Analyzer"
    }
    fn category() -> &'static str {
        "Sources"
    }
    fn color() -> Color32 {
        Color32::from_rgb(100, 75, 140)
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::control::<node_graph::IntSocket>("Sample Rate", |state| {
                &mut state.sample_rate
            }),
            InputDef::control::<node_graph::IntSocket>("Channels", |state| &mut state.channels),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        (0..32_usize)
            .map(|i| OutputDef::new::<Signal>(format!("Ch {i}")))
            .collect()
    }

    fn state() -> Self::State {
        UsbLogicAnalyzerState {
            sample_rate: IntValue::new(100, 1, 1000),
            channels: IntValue::new(16, 1, 32),
        }
    }

    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        let channels = (state.channels.value as usize).clamp(1, 32);
        for (index, output) in outputs.iter_mut().enumerate() {
            output.visible = index < channels;
        }
    }
}

struct SpiDecoder;
impl NodeDef for SpiDecoder {
    type State = SpiDecoderState;

    fn name() -> &'static str {
        "SPI Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 100, 160)
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("CLK"),
            InputDef::new::<Signal>("MOSI"),
            InputDef::new::<Signal>("MISO"),
            InputDef::new::<Signal>("CS"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![
            OutputDef::new::<Protocol>("MOSI"),
            OutputDef::new::<Protocol>("MISO"),
        ]
    }

    fn state() -> Self::State {
        SpiDecoderState {
            mode: EnumValue::new(0, &["Full Duplex", "Half Duplex"]),
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![PropDef::control("mode", "Mode", |state| &mut state.mode)]
    }

    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        if let Some(miso) = outputs.get_mut(1) {
            miso.visible = state.mode.index == 0;
        }
    }
}

struct I2cDecoder;
impl NodeDef for I2cDecoder {
    type State = ();

    fn name() -> &'static str {
        "I2C Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 100, 160)
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("SCL"),
            InputDef::new::<Signal>("SDA"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Protocol>("Data")]
    }

    fn state() -> Self::State {
        ()
    }
}

struct UartDecoder;
impl NodeDef for UartDecoder {
    type State = UartDecoderState;

    fn name() -> &'static str {
        "UART Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 100, 160)
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("TX"),
            InputDef::new::<Signal>("RX"),
            InputDef::control::<node_graph::IntSocket>("Baud Rate", |state| &mut state.baud_rate),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Protocol>("Data")]
    }

    fn state() -> Self::State {
        UartDecoderState {
            baud_rate: IntValue::new(115200, 300, 4_000_000),
            parity: EnumValue::new(0, &["None", "Even", "Odd"]),
            stop_bits: EnumValue::new(0, &["1", "1.5", "2"]),
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![
            PropDef::control("parity", "Parity", |state| &mut state.parity),
            PropDef::control("stop_bits", "Stop Bits", |state| &mut state.stop_bits),
        ]
    }
}

struct ParallelDecoder;
impl NodeDef for ParallelDecoder {
    type State = ParallelDecoderState;

    fn name() -> &'static str {
        "Parallel Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 100, 160)
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("CLK"),
            InputDef::new::<Signal>("D").variadic(32),
            InputDef::control::<node_graph::IntSocket>("Width", |state| &mut state.width),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Protocol>("Data")]
    }

    fn state() -> Self::State {
        ParallelDecoderState {
            width: IntValue::new(8, 1, 32),
        }
    }
}

struct ProtocolFilter;
impl NodeDef for ProtocolFilter {
    type State = ProtocolFilterState;

    fn name() -> &'static str {
        "Protocol Filter"
    }
    fn category() -> &'static str {
        "Filters"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 140, 100)
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Protocol>("Input"),
            InputDef::control::<node_graph::StrSocket>("Pattern", |state| &mut state.pattern),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Protocol>("Output")]
    }

    fn state() -> Self::State {
        ProtocolFilterState {
            pattern: StringValue::new(""),
        }
    }
}

struct TimeWindow;
impl NodeDef for TimeWindow {
    type State = TimeWindowState;

    fn name() -> &'static str {
        "Time Window"
    }
    fn category() -> &'static str {
        "Filters"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 140, 100)
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![InputDef::new::<Protocol>("Input")]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Protocol>("Output")]
    }

    fn state() -> Self::State {
        TimeWindowState {
            start_ms: FloatValue::new(0.0, 0.0, 1e9, 0.1),
            end_ms: FloatValue::new(100.0, 0.0, 1e9, 0.1),
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![
            PropDef::control("start_ms", "Start ms", |state| &mut state.start_ms),
            PropDef::control("end_ms", "End ms", |state| &mut state.end_ms),
        ]
    }
}

struct Viewer;
impl NodeDef for Viewer {
    type State = ();

    fn name() -> &'static str {
        "Viewer"
    }
    fn category() -> &'static str {
        "Output"
    }
    fn color() -> Color32 {
        Color32::from_rgb(160, 80, 60)
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        // Also accepts a raw channel: the viewer renders undecoded signals.
        vec![InputDef::new::<Protocol>("Input").accepts::<Signal>()]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![]
    }

    fn state() -> Self::State {
        ()
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────

fn build_registry() -> NodeTypeRegistry {
    let mut r = NodeTypeRegistry::new();
    r.register::<DslFileSource>();
    r.register::<UsbLogicAnalyzer>();
    r.register::<SpiDecoder>();
    r.register::<I2cDecoder>();
    r.register::<UartDecoder>();
    r.register::<ParallelDecoder>();
    r.register::<ProtocolFilter>();
    r.register::<TimeWindow>();
    r.register::<Viewer>();
    r
}

// ── Demo pipeline ─────────────────────────────────────────────────────────────

fn populate_demo(widget: &mut NodeGraphWidget) {
    use egui::Pos2;

    let id0 = widget
        .add_node_at("DSL File Source", Pos2::new(60.0, 130.0))
        .unwrap();
    let id1 = widget
        .add_node_at("SPI Decoder", Pos2::new(340.0, 80.0))
        .unwrap();
    let id2 = widget
        .add_node_at("Protocol Filter", Pos2::new(610.0, 110.0))
        .unwrap();
    let id3 = widget
        .add_node_at("Viewer", Pos2::new(880.0, 130.0))
        .unwrap();

    let g = widget.graph_mut();
    // Ch 0→CLK, Ch 1→MOSI, Ch 2→MISO, Ch 3→CS
    for (ch, input_idx) in [(0, 0), (1, 1), (2, 2), (3, 3)] {
        g.add_connection(
            SocketId {
                node: id0,
                index: ch,
                direction: SocketDirection::Output,
            },
            SocketId {
                node: id1,
                index: input_idx,
                direction: SocketDirection::Input,
            },
        );
    }
    g.add_connection(
        SocketId {
            node: id1,
            index: 0,
            direction: SocketDirection::Output,
        },
        SocketId {
            node: id2,
            index: 0,
            direction: SocketDirection::Input,
        },
    );
    g.add_connection(
        SocketId {
            node: id2,
            index: 0,
            direction: SocketDirection::Output,
        },
        SocketId {
            node: id3,
            index: 0,
            direction: SocketDirection::Input,
        },
    );
}
