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
//! exists (`crates/logic_analyzer_graph/src/compiler/`). A few node types here have no builder
//! counterpart yet (`dslogic_u3pro16`, `i2c_decoder`): they're editable in
//! the graph but not runnable.

use egui::Color32;

use node_graph::{NodeTypeRegistry, SocketDef, SocketShape};

use super::decoders::I2cDecoder;

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

pub(crate) const COLOR_SOURCES: Color32 = Color32::from_rgb(100, 75, 140);
pub(crate) const COLOR_DECODERS: Color32 = Color32::from_rgb(60, 100, 160);
pub(crate) const COLOR_LOGIC: Color32 = Color32::from_rgb(60, 140, 100);
pub(crate) const COLOR_OUTPUT: Color32 = Color32::from_rgb(160, 80, 60);

// ── Registry ─────────────────────────────────────────────────────────────────

pub fn build_registry() -> NodeTypeRegistry {
    let mut registry = NodeTypeRegistry::new();
    registry.register::<I2cDecoder>();
    for registration in super::graph_node_registrations() {
        assert!(
            registry.category_of(registration.name()).is_none(),
            "graph-node inventory definition '{}' conflicts with an explicit catalog entry",
            registration.name()
        );
        registration.apply_node(&mut registry);
    }
    registry
}

// ── Test graph fixtures ──────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod test_graphs_tests {
    use node_graph::NodeDef;

    use super::super::decoders::{
        BinaryDecoder, BinaryDecoderState, SpiDecoder, SpiDecoderState, UartDecoder,
        UartDecoderState, default_baud_preset, default_display_format, default_input_strategy,
    };
    use super::super::logic::{
        Counter, LogicGate, SrFlipFlop, StringFormatter, WordMatcher, WordMatcherState,
        default_match_op, default_trigger_at,
    };
    use super::super::sinks::{FileWriter, Viewer};
    use super::super::sources::{
        DslFileSource, SigrokFileSource, TestUartSource, TestUartSourceState,
    };

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

    pub(crate) fn build_binary_decoder_demo(widget: &mut node_graph::NodeGraphWidget) {
        use egui::Pos2;
        use node_graph::{BoolValue, EnumValue, IntValue, StringValue};

        let add = |widget: &mut node_graph::NodeGraphWidget, name: &str, x: f32, y: f32| {
            widget
                .add_node_at(name, Pos2::new(x, y))
                .unwrap_or_else(|| panic!("unknown node type '{name}'"))
        };

        let source = add(widget, SigrokFileSource::name(), 40.0, 300.0);
        let mut source_state = SigrokFileSource::state();
        source_state.channels.value = 11;
        source_state.demo_data = true;
        widget.set_node_state(source, serde_json::to_value(source_state).unwrap());
        widget.graph_mut().nodes.get_mut(&source).unwrap().title = SigrokFileSource::name().into();
        let spi = add(widget, SpiDecoder::name(), 360.0, 80.0);
        let start = add(widget, WordMatcher::name(), 680.0, 40.0);
        let stop = add(widget, WordMatcher::name(), 680.0, 230.0);
        let counter = add(widget, Counter::name(), 960.0, 40.0);
        let latch = add(widget, SrFlipFlop::name(), 960.0, 230.0);
        let formatter = add(widget, StringFormatter::name(), 1240.0, 40.0);
        let gate = add(widget, LogicGate::name(), 1198.4297, 592.2656);
        let decoder = add(widget, BinaryDecoder::name(), 1520.0, 300.0);

        widget.set_node_state(
            spi,
            serde_json::to_value(SpiDecoderState {
                metadata: crate::nodes::SpiDecoderMetadata::current(),
                display_format: default_display_format(),
                word_size: IntValue::new(8, 1, 64),
                cpol: EnumValue::new(0, &["0", "1"]),
                cpha: EnumValue::new(0, &["0", "1"]),
                bit_order: EnumValue::new(0, &["MSB first", "LSB first"]),
                cs_polarity: EnumValue::new(0, &["Active low", "Active high", "Disabled"]),
                has_miso: BoolValue::new(true),
            })
            .unwrap(),
        );
        let matcher_state = |pattern: &str| {
            serde_json::to_value(WordMatcherState {
                pattern: StringValue::new(pattern),
                mask: StringValue::new("0xFF"),
                op: default_match_op(),
                trigger_at: default_trigger_at(),
                pulse_output: BoolValue::new(false),
            })
            .unwrap()
        };
        widget.set_node_state(start, matcher_state("0x9A"));
        widget.set_node_state(stop, matcher_state("0xDE"));

        let mut formatter_state = StringFormatter::state();
        formatter_state.template.value = "Window {n:02}".to_owned();
        widget.set_node_state(formatter, serde_json::to_value(formatter_state).unwrap());

        let mut decoder_state = BinaryDecoder::state();
        decoder_state.input_strategy.select("Packed stream");
        widget.set_node_state(decoder, serde_json::to_value(decoder_state).unwrap());

        for (id, title) in [
            (source, "Demo"),
            (start, "Match Start 0x9A"),
            (stop, "Match Stop 0xDE"),
            (gate, "Parallel Enable Gate"),
            (decoder, "Parallel Decoder"),
        ] {
            widget.graph_mut().nodes.get_mut(&id).unwrap().title = title.to_owned();
        }

        connect(widget, (source, "Ch 7"), (spi, "CLK"));
        connect(widget, (source, "Ch 6"), (spi, "MOSI"));
        connect(widget, (source, "Ch 5"), (spi, "MISO"));
        connect(widget, (source, "Ch 8"), (spi, "CS#"));
        connect(widget, (spi, "MOSI Words"), (start, "Words"));
        connect(widget, (spi, "MOSI Words"), (stop, "Words"));
        connect(widget, (start, "Match"), (latch, "Set"));
        connect(widget, (stop, "Match"), (latch, "Reset"));
        connect(widget, (start, "Match"), (counter, "Trigger"));
        connect(widget, (counter, "Count"), (formatter, "Value"));

        connect(widget, (source, "Ch 8"), (gate, "In"));
        connect(widget, (latch, "Q"), (gate, "In"));
        connect(widget, (gate, "Out"), (decoder, "Enable"));
        connect(widget, (source, "Ch 10"), (decoder, "Clock"));
        for bit in 0..8 {
            connect(widget, (source, &format!("Ch {bit}")), (decoder, "D"));
        }
        for (node, output) in [
            (latch, "Q"),
            (gate, "Out"),
            (start, "Match"),
            (stop, "Match"),
            (spi, "MOSI Bits"),
            (spi, "MOSI Data"),
            (spi, "MISO Bits"),
            (spi, "MISO Data"),
            (decoder, "Words"),
            (formatter, "Text"),
        ] {
            let output = output_index(widget, node, output);
            widget.graph_mut().nodes.get_mut(&node).unwrap().outputs[output].show_in_view = true;
        }
        widget
            .graph_mut()
            .nodes
            .get_mut(&formatter)
            .unwrap()
            .selected = true;
    }

    /// Small two-channel graph used by live-capture cursor contract tests.
    pub(crate) fn build_live_binary_test(
        widget: &mut node_graph::NodeGraphWidget,
    ) -> node_graph::NodeId {
        use egui::Pos2;

        use super::super::sources::TestLiveCaptureSource;

        let source = widget
            .add_node_at(TestLiveCaptureSource::name(), Pos2::new(40.0, 80.0))
            .expect("demo source is registered");
        let decoder = widget
            .add_node_at(BinaryDecoder::name(), Pos2::new(360.0, 80.0))
            .expect("binary decoder is registered");
        let mut decoder_state = BinaryDecoder::state();
        decoder_state.input_strategy.select("Packed stream");
        widget.set_node_state(decoder, serde_json::to_value(decoder_state).unwrap());
        connect(widget, (source, "Ch 0"), (decoder, "Clock"));
        connect(widget, (source, "Ch 1"), (decoder, "D"));
        let words = output_index(widget, decoder, "Words");
        widget.graph_mut().nodes.get_mut(&decoder).unwrap().outputs[words].show_in_view = true;
        source
    }

    /// Builds the CCD analysis pipeline captured by the embedded test fixture.
    /// Select a capture in its DSL File Source before running it (SPI cs=8
    /// clk=7 mosi=6; parallel strobe=10 (ACDK), data D0..D7 = ch 0..7).
    ///
    /// The enable gate is `AND(CS, Q)` with no NOT node: CS idles high and the
    /// parallel bus is decodable only while it is *inactive* (channels 6/7 are
    /// multiplexed with SPI), so the raw active-low line already is the
    /// "inverted SPI enable" of the requirement.
    pub(crate) fn populate_startup(widget: &mut node_graph::NodeGraphWidget) {
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

        // Configure states before wiring so `on_update`-driven socket visibility
        // (e.g. hidden MISO) is settled.
        widget.set_node_state(
            spi,
            serde_json::to_value(SpiDecoderState {
                metadata: crate::nodes::SpiDecoderMetadata::current(),
                display_format: default_display_format(),
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
            display_format: default_display_format(),
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
            input_strategy: default_input_strategy(),
            word_size: IntValue::new(1, 1, 8),
            endianness: EnumValue::new(0, &["Little", "Big"]),
            cs_polarity: EnumValue::new(0, &["Disabled", "Active low", "Active high"]),
        };
        decoder_state.sample_on.select("Both (DDR)");
        widget.set_node_state(decoder, serde_json::to_value(decoder_state).unwrap());

        // Descriptive titles (the def is still identified by `type_name`).
        for (id, title) in [
            (start, "Match Start"),
            (stop, "Match Stop"),
            (gate, "Enable Gate"),
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

        // The generic watched-output contract creates the waveform subscription during
        // lowering, keeping presentation choices out of the saved graph's
        // processing topology.
        for (node, output) in [
            (spi, "MOSI Bits"),
            (spi, "MOSI Data"),
            (start, "Match"),
            (stop, "Match"),
            (latch, "Q"),
            (gate, "Out"),
            (decoder, "Words"),
        ] {
            let output = output_index(widget, node, output);
            widget.graph_mut().nodes.get_mut(&node).unwrap().outputs[output].show_in_view = true;
        }
    }

    /// Startup graph for the built-in UART demo. The signal is generated by a
    /// runtime source node; decoded words appear in the viewer only after the
    /// graph runs through the normal pipeline.
    pub(crate) fn populate_uart_demo(widget: &mut node_graph::NodeGraphWidget) {
        use egui::Pos2;
        use node_graph::{BoolValue, EnumValue, IntValue, StringValue};

        let add = |widget: &mut node_graph::NodeGraphWidget, name: &str, x: f32, y: f32| {
            widget
                .add_node_at(name, Pos2::new(x, y))
                .unwrap_or_else(|| panic!("unknown node type '{name}'"))
        };

        let source = add(widget, TestUartSource::name(), 80.0, 220.0);
        let uart = add(widget, UartDecoder::name(), 420.0, 180.0);
        let viewer = add(widget, Viewer::name(), 760.0, 230.0);

        widget.set_node_state(
            source,
            serde_json::to_value(TestUartSourceState {
                message: StringValue::new("HELLO\n"),
                baud_rate: IntValue::new(115_200, 300, 100_000_000),
            })
            .unwrap(),
        );
        widget.set_node_state(
            uart,
            serde_json::to_value(UartDecoderState {
                display_format: default_display_format(),
                baud_preset: default_baud_preset(),
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
        connect(widget, (uart, "Data"), (viewer, "In"));
    }
}

#[cfg(test)]
mod tests {
    use node_graph::{NodeDef, NodeGraphWidget};

    use super::super::sinks::Viewer;
    use super::{build_registry, test_graphs_tests};

    #[test]
    fn startup_graph_builds_with_compatible_wiring() {
        let mut widget = NodeGraphWidget::new(build_registry());
        test_graphs_tests::populate_startup(&mut widget);
        let graph = widget.graph();

        assert_eq!(graph.nodes.len(), 10);
        // src→spi 3, spi→matchers 2, matchers→latch 2, start→counter 1,
        // counter→formatter→writer 2, gate ins 2, gate→decoder 1,
        // clock 1, D0–D7 8, decoder→writer 1. Viewer lanes use the generic
        // watched-output flags and therefore add no saved connections.
        assert_eq!(graph.connections.len(), 23);

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
    fn binary_decoder_demo_fixture_lowers() {
        use crate::{BuilderRegistry, lower};

        let mut widget = NodeGraphWidget::new(build_registry());
        test_graphs_tests::build_binary_decoder_demo(&mut widget);
        assert!(
            widget
                .graph()
                .nodes
                .values()
                .all(|node| node.type_name != Viewer::name())
        );
        assert_eq!(
            widget
                .graph()
                .nodes
                .values()
                .flat_map(|node| &node.outputs)
                .filter(|output| output.show_in_view)
                .count(),
            10
        );
        let preview =
            crate::discover_capture_presentation(widget.graph(), &BuilderRegistry::standard())
                .unwrap()
                .expect("demo source should provide a pre-run capture preview");
        let crate::CapturePresentation::InMemory {
            signals: preview, ..
        } = preview.presentation
        else {
            panic!("demo source should provide an in-memory presentation");
        };
        assert_eq!(preview.len(), 10);
        assert_eq!(preview.first().unwrap().name, "Ch 0");
        assert_eq!(preview.last().unwrap().name, "Ch 10");
        assert_eq!(
            preview.last().unwrap().transitions.last().unwrap().0,
            59_999_000.0
        );
        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .expect("wasm demo should lower cleanly");
        // Watching the formatter output keeps the counter/formatter branch
        // live even though the wasm graph has no filesystem writer sink.
        assert_eq!(widget.graph().nodes.len(), 9);
        assert_eq!(compiled.nodes.len(), 10);
    }

    #[test]
    fn auxiliary_test_graph_fixtures_build_with_registered_nodes() {
        let mut live_binary = NodeGraphWidget::new(build_registry());
        let source = test_graphs_tests::build_live_binary_test(&mut live_binary);
        assert!(live_binary.graph().nodes.contains_key(&source));
        assert_eq!(live_binary.graph().nodes.len(), 2);

        let mut uart = NodeGraphWidget::new(build_registry());
        test_graphs_tests::populate_uart_demo(&mut uart);
        assert_eq!(uart.graph().nodes.len(), 3);
        assert_eq!(uart.graph().connections.len(), 3);
    }

    #[test]
    fn graph_file_api_round_trips_the_startup_graph() {
        let mut original = NodeGraphWidget::new(build_registry());
        test_graphs_tests::populate_startup(&mut original);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        original.save_to_path(&path).unwrap();

        let mut loaded = NodeGraphWidget::new(build_registry());
        loaded.load_from_path(&path).unwrap();

        assert_eq!(loaded.graph().nodes.len(), 10);
        assert_eq!(loaded.graph().connections.len(), 23);
    }

    /// Save/load round-trip: serializing the graph to JSON and
    /// restoring it through the registry (the File > Save/Load path) must
    /// compile to the same pipeline.
    #[test]
    fn graph_json_round_trip_compiles_identically() {
        use crate::{BuilderRegistry, lower};

        let mut widget = NodeGraphWidget::new(build_registry());
        test_graphs_tests::populate_startup(&mut widget);
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
        let edges = |compiled: &crate::CompiledGraph| {
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
}
