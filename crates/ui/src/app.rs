use egui::Color32;
use node_graph::{
    EnumValue, FloatValue, InputDef, IntValue, NodeDef, NodeGraphWidget, NodeTypeRegistry,
    OutputDef, PropDef, Socket, SocketDef, SocketDirection, SocketId, SocketShape, StringValue,
};
use serde::{Deserialize, Serialize};

pub struct App {
    node_graph: NodeGraphWidget,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext) -> Self {
        let registry = build_registry();
        let mut widget = NodeGraphWidget::new(registry);
        populate_demo(&mut widget);
        Self { node_graph: widget }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.node_graph.show(ui);
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
    file: StringValue,
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
            InputDef::control::<node_graph::StrSocket>("File", |state| &mut state.file),
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
            file: StringValue::new(""),
            channels: IntValue::new(8, 1, 32),
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
        vec![InputDef::new::<Protocol>("Input")]
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
