use egui::Color32;
use node_graph::{
    EnumValue, FloatValue, InputDef, InputSocket, IntValue, NodeDef, NodeGraphWidget,
    NodeTypeRegistry, OutputDef, Prop, PropDef, Socket, SocketId, SocketShape, SocketTypeDef,
    StringValue,
};

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
impl SocketTypeDef for Signal {
    fn type_name() -> &'static str {
        "Signal"
    }
    fn color() -> Color32 {
        Color32::from_rgb(80, 200, 80)
    }
}

struct Protocol;
impl SocketTypeDef for Protocol {
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

// ── Node type definitions ─────────────────────────────────────────────────────

struct DslFileSource;
impl NodeDef for DslFileSource {
    fn name() -> &'static str {
        "DSL File Source"
    }
    fn category() -> &'static str {
        "Sources"
    }
    fn color() -> Color32 {
        Color32::from_rgb(100, 75, 140)
    }

    fn inputs() -> Vec<InputDef> {
        vec![
            InputDef::with_value::<node_graph::StrSocket>("File", StringValue::new("").into()),
            InputDef::with_value::<node_graph::IntSocket>("Channel Mask", IntValue::plain(0xFF)),
        ]
    }

    fn outputs() -> Vec<OutputDef> {
        vec![OutputDef::new::<Signal>("Signal")]
    }
}

struct UsbLogicAnalyzer;
impl NodeDef for UsbLogicAnalyzer {
    fn name() -> &'static str {
        "USB Logic Analyzer"
    }
    fn category() -> &'static str {
        "Sources"
    }
    fn color() -> Color32 {
        Color32::from_rgb(100, 75, 140)
    }

    fn inputs() -> Vec<InputDef> {
        vec![
            InputDef::with_value::<node_graph::IntSocket>("Sample Rate", IntValue::new(100, 1, 1000).into()),
            InputDef::with_value::<node_graph::IntSocket>("Channels",    IntValue::new(16,  1,  32).into()),
        ]
    }

    fn outputs() -> Vec<OutputDef> {
        vec![OutputDef::new::<Signal>("Signal")]
    }
}

struct SpiDecoder;
impl NodeDef for SpiDecoder {
    fn name() -> &'static str {
        "SPI Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 100, 160)
    }

    fn inputs() -> Vec<InputDef> {
        vec![
            InputDef::new::<Signal>("Signal"),
            InputDef::with_value::<node_graph::IntSocket>("CLK Chan",  IntValue::new(0, 0, 31).into()),
            InputDef::with_value::<node_graph::IntSocket>("MOSI Chan", IntValue::new(1, 0, 31).into()),
            InputDef::with_value::<node_graph::IntSocket>("MISO Chan", IntValue::new(2, 0, 31).into()),
            InputDef::with_value::<node_graph::IntSocket>("CS Chan",   IntValue::new(3, 0, 31).into()),
        ]
    }

    fn outputs() -> Vec<OutputDef> {
        vec![
            OutputDef::new::<Protocol>("MOSI"),
            OutputDef::new::<Protocol>("MISO"),
        ]
    }

    fn props() -> Vec<PropDef> {
        vec![PropDef::new("mode", "Mode", EnumValue::new(0, &["Full Duplex", "Half Duplex"]).into())]
    }

    fn on_update() -> Option<fn(&mut [InputSocket], &mut [Socket], &[Prop])> {
        Some(|_, outputs, props| {
            let full_duplex = props[0].value.as_any()
                .downcast_ref::<node_graph::EnumValue>()
                .is_none_or(|e| e.index == 0);
            if let Some(miso) = outputs.get_mut(1) {
                miso.visible = full_duplex;
            }
        })
    }
}

struct I2cDecoder;
impl NodeDef for I2cDecoder {
    fn name() -> &'static str {
        "I2C Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 100, 160)
    }

    fn inputs() -> Vec<InputDef> {
        vec![
            InputDef::new::<Signal>("Signal"),
            InputDef::with_value::<node_graph::IntSocket>("SCL Chan", IntValue::new(0, 0, 31).into()),
            InputDef::with_value::<node_graph::IntSocket>("SDA Chan", IntValue::new(1, 0, 31).into()),
        ]
    }

    fn outputs() -> Vec<OutputDef> {
        vec![OutputDef::new::<Protocol>("Data")]
    }
}

struct UartDecoder;
impl NodeDef for UartDecoder {
    fn name() -> &'static str {
        "UART Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 100, 160)
    }

    fn inputs() -> Vec<InputDef> {
        vec![
            InputDef::new::<Signal>("Signal"),
            InputDef::with_value::<node_graph::IntSocket>("TX Chan",   IntValue::new(0, 0, 31).into()),
            InputDef::with_value::<node_graph::IntSocket>("RX Chan",   IntValue::new(1, 0, 31).into()),
            InputDef::with_value::<node_graph::IntSocket>("Baud Rate", IntValue::new(115200, 300, 4_000_000).into()),
        ]
    }

    fn outputs() -> Vec<OutputDef> {
        vec![OutputDef::new::<Protocol>("Data")]
    }

    fn props() -> Vec<PropDef> {
        vec![
            PropDef::new("parity",    "Parity",    EnumValue::new(0, &["None", "Even", "Odd"]).into()),
            PropDef::new("stop_bits", "Stop Bits", EnumValue::new(0, &["1", "1.5", "2"]).into()),
        ]
    }
}

struct ParallelDecoder;
impl NodeDef for ParallelDecoder {
    fn name() -> &'static str {
        "Parallel Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 100, 160)
    }

    fn inputs() -> Vec<InputDef> {
        vec![
            InputDef::new::<Signal>("Signal"),
            InputDef::with_value::<node_graph::IntSocket>("Channels", IntValue::new(8, 1, 32).into()),
            InputDef::with_value::<node_graph::IntSocket>("CLK Chan", IntValue::new(0, 0, 31).into()),
        ]
    }

    fn outputs() -> Vec<OutputDef> {
        vec![OutputDef::new::<Protocol>("Data")]
    }
}

struct ProtocolFilter;
impl NodeDef for ProtocolFilter {
    fn name() -> &'static str {
        "Protocol Filter"
    }
    fn category() -> &'static str {
        "Filters"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 140, 100)
    }

    fn inputs() -> Vec<InputDef> {
        vec![
            InputDef::new::<Protocol>("Input"),
            InputDef::with_value::<node_graph::StrSocket>("Pattern", StringValue::new("").into()),
        ]
    }

    fn outputs() -> Vec<OutputDef> {
        vec![OutputDef::new::<Protocol>("Output")]
    }
}

struct TimeWindow;
impl NodeDef for TimeWindow {
    fn name() -> &'static str {
        "Time Window"
    }
    fn category() -> &'static str {
        "Filters"
    }
    fn color() -> Color32 {
        Color32::from_rgb(60, 140, 100)
    }

    fn inputs() -> Vec<InputDef> {
        vec![InputDef::new::<Protocol>("Input")]
    }

    fn outputs() -> Vec<OutputDef> {
        vec![OutputDef::new::<Protocol>("Output")]
    }

    fn props() -> Vec<PropDef> {
        vec![
            PropDef::new("start_ms", "Start ms", FloatValue::new(0.0,   0.0, 1e9, 0.1).into()),
            PropDef::new("end_ms",   "End ms",   FloatValue::new(100.0, 0.0, 1e9, 0.1).into()),
        ]
    }
}

struct Viewer;
impl NodeDef for Viewer {
    fn name() -> &'static str {
        "Viewer"
    }
    fn category() -> &'static str {
        "Output"
    }
    fn color() -> Color32 {
        Color32::from_rgb(160, 80, 60)
    }

    fn inputs() -> Vec<InputDef> {
        vec![InputDef::new::<Protocol>("Input")]
    }

    fn outputs() -> Vec<OutputDef> {
        vec![]
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
    g.add_connection(
        SocketId {
            node: id0,
            index: 0,
            is_output: true,
        },
        SocketId {
            node: id1,
            index: 0,
            is_output: false,
        },
    );
    g.add_connection(
        SocketId {
            node: id1,
            index: 0,
            is_output: true,
        },
        SocketId {
            node: id2,
            index: 0,
            is_output: false,
        },
    );
    g.add_connection(
        SocketId {
            node: id2,
            index: 0,
            is_output: true,
        },
        SocketId {
            node: id3,
            index: 0,
            is_output: false,
        },
    );
}
