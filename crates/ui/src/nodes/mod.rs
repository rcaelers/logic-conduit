//! Node definitions for the analysis-pipeline editor.
//!
//! Socket styling follows `docs/APP_DESIGN.md`: the shape
//! encodes how a value exists in time (■ static config, ● level stream,
//! ◆ event stream) and the color encodes the payload family, shared across
//! shapes (green logic, amber pulse, orange words, blue integer, rose text).
//! Red is reserved for error feedback, grey for the wildcard.
//!
//! Prop placement follows `docs/APP_DESIGN.md`: the node body carries sockets and the
//! controls someone tweaks while reading the graph; everything else lives in
//! the properties panel (N).
//!
//! One file per node type, named to match its `compiler` builder where one
//! exists (`crates/ui/src/compiler/`). A few node types here have no builder
//! counterpart yet (`dslogic_u3pro16`, `i2c_decoder`): they're editable in
//! the graph but not runnable.

use egui::Color32;
use node_graph::{NodeDef, NodeTypeRegistry, SocketDef, SocketShape};

mod binary_decoder;
mod buffer;
mod counter;
#[cfg(not(target_arch = "wasm32"))]
mod csv_writer;
mod dslogic_u3pro16;
mod file_source;
mod file_writer;
mod formatter;
mod i2c_decoder;
mod logic_gate;
#[cfg(not(target_arch = "wasm32"))]
mod sigrok_file_source;
mod spi_decoder;
mod sr_flip_flop;
#[cfg(not(target_arch = "wasm32"))]
mod text_file_writer;
mod tgck_recorder;
mod uart_decoder;
mod uart_demo_source;
mod viewer;
mod word_matcher;

pub use binary_decoder::{BinaryDecoder, BinaryDecoderState};
pub use buffer::{Buffer, BufferState};
pub use counter::{Counter, CounterState};
#[cfg(not(target_arch = "wasm32"))]
pub use csv_writer::{CsvWriter, CsvWriterState};
pub use dslogic_u3pro16::DsLogicU3Pro16;
pub use file_source::DslFileSource;
#[cfg(not(target_arch = "wasm32"))]
pub use file_source::DslFileSourceState;
pub use file_writer::FileWriter;
#[cfg(not(target_arch = "wasm32"))]
pub use file_writer::FileWriterState;
pub use formatter::{StringFormatter, StringFormatterState};
pub use i2c_decoder::I2cDecoder;
pub use logic_gate::{LogicGate, LogicGateState};
#[cfg(not(target_arch = "wasm32"))]
pub use sigrok_file_source::{SigrokFileSource, SigrokFileSourceState};
pub use spi_decoder::{SpiDecoder, SpiDecoderState};
pub use sr_flip_flop::{SrFlipFlop, SrFlipFlopState};
#[cfg(not(target_arch = "wasm32"))]
pub use text_file_writer::TextFileWriter;
pub use tgck_recorder::TgckRecorder;
pub use uart_decoder::{UartDecoder, UartDecoderState};
pub use uart_demo_source::{UartDemoSource, UartDemoSourceState};
pub use viewer::{Viewer, ViewerState};
pub use word_matcher::{WordMatcher, WordMatcherState, default_match_op, default_trigger_at};

// ── Stream socket types (`docs/APP_DESIGN.md`) ───────────────────────────────────────────────

/// Logic level stream (`Sample` at runtime): defined at every instant.
pub struct Signal;
impl SocketDef for Signal {
    type Value = bool;

    fn type_name() -> &'static str {
        "Signal"
    }
    fn color() -> Color32 {
        Color32::from_rgb(0, 205, 160)
    }
}

/// Decoded word events (`Word` at runtime).
pub struct Words;
impl SocketDef for Words {
    type Value = u64;

    fn type_name() -> &'static str {
        "Words"
    }
    fn color() -> Color32 {
        Color32::from_rgb(215, 140, 60)
    }
    fn shape() -> SocketShape {
        SocketShape::Diamond
    }
}

/// Instantaneous events with no payload beyond time (`Trigger` at runtime).
pub struct Trigger;
impl SocketDef for Trigger {
    type Value = ();

    fn type_name() -> &'static str {
        "Trigger"
    }
    fn color() -> Color32 {
        Color32::from_rgb(230, 190, 80)
    }
    fn shape() -> SocketShape {
        SocketShape::Diamond
    }
}

/// Integer level stream (`NumberSample` at runtime).
pub struct Number;
impl SocketDef for Number {
    type Value = i64;

    fn type_name() -> &'static str {
        "Number"
    }
    fn color() -> Color32 {
        Color32::from_rgb(95, 145, 210)
    }
}

/// Text level stream (`TextSample` at runtime).
pub struct Text;
impl SocketDef for Text {
    type Value = String;

    fn type_name() -> &'static str {
        "Text"
    }
    fn color() -> Color32 {
        Color32::from_rgb(215, 150, 170)
    }
}

/// A [`Text`] input (same wire type, so any `Text` output connects) whose
/// inline control — shown in the node body only while the socket is
/// unconnected — is a save-file picker instead of a plain string field.
pub struct TextSavePath;
impl SocketDef for TextSavePath {
    type Value = String;

    fn type_name() -> &'static str {
        Text::type_name()
    }
    fn color() -> Color32 {
        Text::color()
    }
}
impl node_graph::SocketWithControlDef for TextSavePath {
    type Control = node_graph::FileValue;
}

/// The open-dialog counterpart of [`TextSavePath`]: a [`Text`] input whose
/// inline control (shown while unconnected) is an open-file picker — pick
/// an existing file, or wire a text filename in.
pub struct TextOpenPath;
impl SocketDef for TextOpenPath {
    type Value = String;

    fn type_name() -> &'static str {
        Text::type_name()
    }
    fn color() -> Color32 {
        Text::color()
    }
}
impl node_graph::SocketWithControlDef for TextOpenPath {
    type Control = node_graph::FileValue;
}

// ── Category colors ──────────────────────────────────────────────────────────

const COLOR_SOURCES: Color32 = Color32::from_rgb(100, 75, 140);
const COLOR_DECODERS: Color32 = Color32::from_rgb(60, 100, 160);
const COLOR_LOGIC: Color32 = Color32::from_rgb(60, 140, 100);
const COLOR_OUTPUT: Color32 = Color32::from_rgb(160, 80, 60);

// ── Registry ─────────────────────────────────────────────────────────────────

pub fn build_registry() -> NodeTypeRegistry {
    let mut registry = NodeTypeRegistry::new();
    // File-backed node types are native-only (no filesystem in the
    // browser) — gated here too, not just in `compiler::BuilderRegistry`,
    // so a wasm build never lets a node be added that can't compile.
    #[cfg(not(target_arch = "wasm32"))]
    registry.register::<DslFileSource>();
    #[cfg(not(target_arch = "wasm32"))]
    registry.register::<SigrokFileSource>();
    registry.register::<UartDemoSource>();
    registry.register::<DsLogicU3Pro16>();
    registry.register::<SpiDecoder>();
    registry.register::<UartDecoder>();
    registry.register::<I2cDecoder>();
    registry.register::<BinaryDecoder>();
    registry.register::<WordMatcher>();
    registry.register::<SrFlipFlop>();
    registry.register::<LogicGate>();
    registry.register::<Buffer>();
    registry.register::<Counter>();
    registry.register::<StringFormatter>();
    #[cfg(not(target_arch = "wasm32"))]
    registry.register::<FileWriter>();
    #[cfg(not(target_arch = "wasm32"))]
    registry.register::<TextFileWriter>();
    #[cfg(not(target_arch = "wasm32"))]
    registry.register::<CsvWriter>();
    registry.register::<TgckRecorder>();
    registry.register::<Viewer>();
    registry
}

// ── Startup graph: the CCD capture pipeline ─────────────────────────────

fn output_index(
    widget: &node_graph::NodeGraphWidget,
    node: node_graph::NodeId,
    name: &str,
) -> usize {
    widget.graph().nodes[&node]
        .outputs
        .iter()
        .position(|socket| socket.name == name)
        .unwrap_or_else(|| panic!("no output socket '{name}'"))
}

fn input_index(
    widget: &node_graph::NodeGraphWidget,
    node: node_graph::NodeId,
    name: &str,
) -> usize {
    widget.graph().nodes[&node]
        .inputs
        .iter()
        .position(|socket| socket.name == name && socket.visible)
        .unwrap_or_else(|| panic!("no input socket '{name}'"))
}

fn connect(
    widget: &mut node_graph::NodeGraphWidget,
    from: (node_graph::NodeId, &str),
    to: (node_graph::NodeId, &str),
) {
    let from_socket = node_graph::SocketId {
        node: from.0,
        index: output_index(widget, from.0, from.1),
        direction: node_graph::SocketDirection::Output,
    };
    let to_socket = node_graph::SocketId {
        node: to.0,
        index: input_index(widget, to.0, to.1),
        direction: node_graph::SocketDirection::Input,
    };
    widget.graph_mut().add_connection(from_socket, to_socket);
}

/// Builds the CCD analysis pipeline (`graphs/ccd_pipeline.json`) as
/// the startup graph, wired for `_captures/wipneus5.dsl` (SPI cs=8 clk=7
/// mosi=6; parallel strobe=10 (ACDK), data D0..D7 = ch 0..7).
///
/// The enable gate is `AND(CS, Q)` with no NOT node: CS idles high and the
/// parallel bus is decodable only while it is *inactive* (channels 6/7 are
/// multiplexed with SPI), so the raw active-low line already is the
/// "inverted SPI enable" of the requirement.
#[cfg_attr(not(test), allow(dead_code))]
pub fn populate_startup(widget: &mut node_graph::NodeGraphWidget) {
    use egui::Pos2;
    use node_graph::{BoolValue, EnumValue, IntValue, StringValue};

    let add = |widget: &mut node_graph::NodeGraphWidget, name: &str, x: f32, y: f32| {
        widget
            .add_node_at(name, Pos2::new(x, y))
            .unwrap_or_else(|| panic!("unknown node type '{name}'"))
    };

    let source = add(widget, DslFileSource::name(), 40.0, 260.0);
    let spi = add(widget, SpiDecoder::name(), 330.0, 120.0);
    let start = add(widget, WordMatcher::name(), 620.0, 40.0);
    let stop = add(widget, WordMatcher::name(), 620.0, 230.0);
    let counter = add(widget, Counter::name(), 900.0, 40.0);
    let latch = add(widget, SrFlipFlop::name(), 900.0, 230.0);
    let formatter = add(widget, StringFormatter::name(), 1160.0, 40.0);
    let gate = add(widget, LogicGate::name(), 1160.0, 400.0);
    let decoder = add(widget, BinaryDecoder::name(), 1440.0, 260.0);
    let writer = add(widget, FileWriter::name(), 1760.0, 120.0);
    let viewer_buffer = add(widget, Buffer::name(), 1760.0, 420.0);
    let viewer = add(widget, Viewer::name(), 2020.0, 420.0);

    // Configure states before wiring so `on_update`-driven socket visibility
    // (e.g. hidden MISO) is settled.
    widget.set_node_state(
        spi,
        serde_json::to_value(SpiDecoderState {
            word_size: IntValue::new(24, 1, 32),
            cpol: EnumValue::new(0, &["0", "1"]),
            cpha: EnumValue::new(0, &["0", "1"]),
            bit_order: EnumValue::new(0, &["MSB first", "LSB first"]),
            cs_polarity: EnumValue::new(0, &["Active low", "Active high", "Disabled"]),
            has_miso: BoolValue::new(false),
        })
        .unwrap(),
    );
    let matcher_state = |pattern: &str| {
        serde_json::to_value(WordMatcherState {
            pattern: StringValue::new(pattern),
            mask: StringValue::new("0xFFFFFF"),
            op: default_match_op(),
            trigger_at: default_trigger_at(),
            pulse_output: BoolValue::new(false),
        })
        .unwrap()
    };
    widget.set_node_state(start, matcher_state("0x600081"));
    widget.set_node_state(stop, matcher_state("0x600000"));
    let mut decoder_state = BinaryDecoderState {
        sample_on: EnumValue::new(
            0,
            &[
                "Rising (SDR)",
                "Falling (SDR)",
                "Both (DDR)",
                "High level",
                "Low level",
            ],
        ),
        input_strategy: binary_decoder::default_input_strategy(),
        word_size: IntValue::new(1, 1, 8),
        endianness: EnumValue::new(0, &["Little", "Big"]),
        cs_polarity: EnumValue::new(0, &["Disabled", "Active low", "Active high"]),
    };
    decoder_state.sample_on.select("Both (DDR)");
    widget.set_node_state(decoder, serde_json::to_value(decoder_state).unwrap());

    // Decoupling point (`docs/APP_DESIGN.md`, buffer policy): the viewer's
    // own backpressure must not stall the file writer sharing this same
    // decoder output, so only the viewer branch gets an explicit buffer —
    // the writer stays directly connected to get full-rate, undelayed data.
    let mut buffer_state = Buffer::state();
    buffer_state.kind.select("Word");
    buffer_state.capacity.value = 100_000;
    widget.set_node_state(viewer_buffer, serde_json::to_value(buffer_state).unwrap());

    // Descriptive titles (the def is still identified by `type_name`).
    for (id, title) in [
        (start, "Match Start"),
        (stop, "Match Stop"),
        (gate, "Enable Gate"),
        (viewer_buffer, "Viewer Buffer"),
    ] {
        if let Some(node) = widget.graph_mut().nodes.get_mut(&id) {
            node.title = title.to_owned();
        }
    }

    // SPI control path.
    connect(widget, (source, "Ch 7"), (spi, "CLK"));
    connect(widget, (source, "Ch 6"), (spi, "MOSI"));
    connect(widget, (source, "Ch 8"), (spi, "CS#"));
    connect(widget, (spi, "MOSI Words"), (start, "Words"));
    connect(widget, (spi, "MOSI Words"), (stop, "Words"));
    connect(widget, (start, "Match"), (latch, "Set"));
    connect(widget, (stop, "Match"), (latch, "Reset"));

    // Filename path.
    connect(widget, (start, "Match"), (counter, "Trigger"));
    connect(widget, (counter, "Count"), (formatter, "Value"));
    connect(widget, (formatter, "Text"), (writer, "Filename"));

    // Enable gate: stream window (Q) AND bus free (CS inactive-high).
    connect(widget, (source, "Ch 8"), (gate, "In"));
    connect(widget, (latch, "Q"), (gate, "In"));
    connect(widget, (gate, "Out"), (decoder, "Enable"));

    // Data path.
    connect(widget, (source, "Ch 10"), (decoder, "Clock"));
    for bit in 0..8 {
        connect(widget, (source, &format!("Ch {bit}")), (decoder, "D"));
    }
    connect(widget, (decoder, "Words"), (writer, "Data"));
    connect(widget, (decoder, "Words"), (viewer_buffer, "In"));

    // Viewer lanes: flip-flop state, enable, both triggers, decoded words
    // (the last via `viewer_buffer`, wired above — lane order follows
    // connection order, so this stays last to match the pre-buffer lane
    // order tests assert on).
    connect(widget, (latch, "Q"), (viewer, "In"));
    connect(widget, (gate, "Out"), (viewer, "In"));
    connect(widget, (start, "Match"), (viewer, "In"));
    connect(widget, (stop, "Match"), (viewer, "In"));
    connect(widget, (viewer_buffer, "Out"), (viewer, "In"));
}

/// Startup graph for the built-in UART demo. The signal is generated by a
/// runtime source node; decoded words appear in the viewer only after the
/// graph runs through the normal pipeline.
#[cfg_attr(not(any(test, target_arch = "wasm32")), allow(dead_code))]
pub fn populate_uart_demo(widget: &mut node_graph::NodeGraphWidget) {
    use egui::Pos2;
    use node_graph::{BoolValue, EnumValue, IntValue, StringValue};

    let add = |widget: &mut node_graph::NodeGraphWidget, name: &str, x: f32, y: f32| {
        widget
            .add_node_at(name, Pos2::new(x, y))
            .unwrap_or_else(|| panic!("unknown node type '{name}'"))
    };

    let source = add(widget, UartDemoSource::name(), 80.0, 220.0);
    let uart = add(widget, UartDecoder::name(), 420.0, 180.0);
    let viewer = add(widget, Viewer::name(), 760.0, 230.0);

    widget.set_node_state(
        source,
        serde_json::to_value(UartDemoSourceState {
            message: StringValue::new("HELLO\n"),
            baud_rate: IntValue::new(115_200, 300, 100_000_000),
        })
        .unwrap(),
    );
    widget.set_node_state(
        uart,
        serde_json::to_value(UartDecoderState {
            baud_rate: IntValue::new(115_200, 300, 100_000_000),
            data_bits: IntValue::new(8, 5, 9),
            parity: EnumValue::new(0, &["None", "Odd", "Even", "Mark", "Space"]),
            check_parity: BoolValue::new(false),
            stop_bits: EnumValue::new(2, &["0", "0.5", "1", "1.5", "2"]),
            bit_order: EnumValue::new(0, &["LSB first", "MSB first"]),
            invert: BoolValue::new(false),
            error_output: BoolValue::new(false),
        })
        .unwrap(),
    );

    if let Some(node) = widget.graph_mut().nodes.get_mut(&source) {
        node.title = "Generated serial.rx".to_owned();
    }
    if let Some(node) = widget.graph_mut().nodes.get_mut(&uart) {
        node.title = "UART 115200 8N1".to_owned();
    }

    connect(widget, (source, "RX"), (uart, "RX/TX"));
    connect(widget, (source, "RX"), (viewer, "In"));
    connect(widget, (uart, "Words"), (viewer, "In"));
}

/// Path of the first DSL File Source node with a non-empty file, for the
/// logic-analyzer view. Native-only: `DslFileSource` itself isn't
/// registered on wasm (no filesystem), so no graph can ever have one there.
#[cfg(not(target_arch = "wasm32"))]
pub fn dsl_file_source_path(graph: &node_graph::GraphState) -> Option<String> {
    graph
        .nodes
        .values()
        .filter(|node| node.def_name() == DslFileSource::name())
        .filter_map(|node| serde_json::from_value::<DslFileSourceState>(node.state.clone()).ok())
        .map(|state| state.file.value)
        .find(|path| !path.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use node_graph::NodeGraphWidget;

    #[test]
    fn checked_in_spi_decode_pipeline_lowers_cleanly() {
        use crate::compiler::{BuilderRegistry, lower};

        let saved: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../graphs/spi_decode_pipeline.json"
        ))
        .expect("checked-in graph should be valid JSON");
        let graph: node_graph::GraphState =
            serde_json::from_value(saved).expect("checked-in graph should deserialize");

        let mut widget = NodeGraphWidget::new(build_registry());
        widget.set_graph(graph);

        let registry = BuilderRegistry::standard();
        let compiled = lower(widget.graph(), &registry).expect("graph should lower cleanly");
        assert_eq!(compiled.nodes.len(), 4);
    }

    #[test]
    fn checked_in_spi_graph_decode_pipeline_lowers_cleanly() {
        use crate::compiler::{BuilderRegistry, lower};

        let saved: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../graphs/spi_graph_decode_pipeline.json"
        ))
        .expect("checked-in graph should be valid JSON");
        let graph: node_graph::GraphState =
            serde_json::from_value(saved).expect("checked-in graph should deserialize");

        let mut widget = NodeGraphWidget::new(build_registry());
        widget.set_graph(graph);

        let registry = BuilderRegistry::standard();
        let compiled = lower(widget.graph(), &registry).expect("graph should lower cleanly");
        assert_eq!(compiled.nodes.len(), 9);
    }

    #[test]
    fn startup_graph_builds_with_compatible_wiring() {
        let mut widget = NodeGraphWidget::new(build_registry());
        populate_startup(&mut widget);
        let graph = widget.graph();

        assert_eq!(graph.nodes.len(), 12);
        // src→spi 3, spi→matchers 2, matchers→latch 2, start→counter 1,
        // counter→formatter→writer 2, gate ins 2, gate→decoder 1,
        // clock 1, D0–D7 8, decoder→writer 1, decoder→buffer 1,
        // viewer lanes 5 (one of which is buffer→viewer).
        assert_eq!(graph.connections.len(), 29);

        for connection in &graph.connections {
            let from = &graph.nodes[&connection.from.node].outputs[connection.from.index];
            let to = &graph.nodes[&connection.to.node].inputs[connection.to.index];
            assert!(
                to.accepts(from.effective_type()),
                "incompatible wire {} ({}) -> {} ({})",
                from.name,
                from.effective_type(),
                to.name,
                to.type_name,
            );
        }
    }

    #[test]
    fn checked_in_ccd_graph_matches_builder() {
        let saved: serde_json::Value =
            serde_json::from_str(include_str!("../../../../graphs/ccd_pipeline.json"))
                .expect("checked-in CCD graph should be valid JSON");

        let mut widget = NodeGraphWidget::new(build_registry());
        populate_startup(&mut widget);
        let generated = serde_json::to_value(widget.graph()).expect("graph should serialize");

        assert_eq!(saved, generated);
    }

    #[test]
    fn graph_file_api_round_trips_the_startup_graph() {
        let mut original = NodeGraphWidget::new(build_registry());
        populate_startup(&mut original);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        original.save_to_path(&path).unwrap();

        let mut loaded = NodeGraphWidget::new(build_registry());
        loaded.load_from_path(&path).unwrap();

        assert_eq!(loaded.graph().nodes.len(), 12);
        assert_eq!(loaded.graph().connections.len(), 29);
    }

    /// Save/load round-trip: serializing the graph to JSON and
    /// restoring it through the registry (the File > Save/Load path) must
    /// compile to the same pipeline.
    #[test]
    fn graph_json_round_trip_compiles_identically() {
        use crate::compiler::{BuilderRegistry, lower};

        let mut widget = NodeGraphWidget::new(build_registry());
        populate_startup(&mut widget);
        let registry = BuilderRegistry::standard();
        let original = lower(widget.graph(), &registry).expect("original lowers");

        let json = serde_json::to_string(widget.graph()).expect("graph serializes");
        let restored_state: node_graph::GraphState =
            serde_json::from_str(&json).expect("graph deserializes");
        let mut restored = NodeGraphWidget::new(build_registry());
        restored.set_graph(restored_state);

        let reloaded = lower(restored.graph(), &registry).expect("restored lowers");

        assert_eq!(original.nodes.len(), reloaded.nodes.len());
        for (a, b) in original.nodes.iter().zip(&reloaded.nodes) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.builder, b.builder);
            assert_eq!(
                a.state, b.state,
                "state of {} changed in round-trip",
                a.builder
            );
        }
        let edges = |compiled: &crate::compiler::CompiledGraph| {
            let mut edges: Vec<String> = compiled
                .edges
                .iter()
                .map(|edge| {
                    format!(
                        "n{}:{} -> n{}:{} ({})",
                        edge.from.0.0, edge.from.1, edge.to.0.0, edge.to.1, edge.buffer
                    )
                })
                .collect();
            edges.sort();
            edges
        };
        assert_eq!(edges(&original), edges(&reloaded));
    }

    #[test]
    fn dsl_source_path_found_by_def_name_after_rename() {
        let mut widget = NodeGraphWidget::new(build_registry());
        populate_startup(&mut widget);
        let source_id = *widget
            .graph()
            .nodes
            .iter()
            .find(|(_, node)| node.def_name() == DslFileSource::name())
            .map(|(id, _)| id)
            .unwrap();
        widget.graph_mut().nodes.get_mut(&source_id).unwrap().title = "My capture".to_owned();
        assert_eq!(
            dsl_file_source_path(widget.graph()).as_deref(),
            Some("_captures/wipneus5.dsl")
        );
    }
}
