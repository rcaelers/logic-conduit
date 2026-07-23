use egui::Pos2;

use logic_analyzer_graph::host::GraphCompiler;
use logic_analyzer_graph_api::node_support::{CapturePresentation, LiveCaptureEdit};
use node_graph::{NodeGraphWidget, SocketDirection, SocketId};
use signal_processing::{CaptureChannelId, CaptureDataDelivery, SimpleTriggerCondition};

use super::{node_name, test_graphs_tests};
use crate::test_support::build_registry;

const U3PRO16_ID: &str = "org.logicconduit.graph-node.dslogic-u3pro16/v1";
const DSL_FILE_SOURCE_ID: &str = "org.logicconduit.graph-node.dsl-file-source/v1";
const VIEWER_ID: &str = "org.logicconduit.graph-node.viewer/v1";

fn select(state: &mut serde_json::Value, field: &str, value: &str) {
    state[field]["value"] = serde_json::Value::String(value.to_owned());
}

fn enable_channels(state: &mut serde_json::Value, channels: &[usize]) {
    let enabled = state["channels"]["enabled"]
        .as_array_mut()
        .expect("capture source channels are an array");
    enabled.fill(serde_json::Value::Bool(false));
    for &channel in channels {
        enabled[channel] = serde_json::Value::Bool(true);
    }
}

fn attach_viewer_sink(widget: &mut NodeGraphWidget, source: node_graph::NodeId) {
    let viewer = widget
        .add_node_at(node_name(VIEWER_ID), Pos2::new(320.0, 0.0))
        .expect("viewer should be registered");
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
}

#[test]
fn native_hardware_source_registers_and_lowers() {
    let mut widget = NodeGraphWidget::new(build_registry());
    let source = widget
        .add_node_at(node_name(U3PRO16_ID), Pos2::ZERO)
        .expect("native hardware source should be registered");
    attach_viewer_sink(&mut widget, source);

    let compiled = GraphCompiler::new().lower(widget.graph()).unwrap();
    assert!(
        compiled
            .nodes
            .iter()
            .any(|node| node.builder == node_name(U3PRO16_ID))
    );
}

#[test]
fn buffered_hardware_feature_lowers_opaque_channels_and_portable_trigger_edits() {
    let mut widget = NodeGraphWidget::new(build_registry());
    let source = widget
        .add_node_at(node_name(U3PRO16_ID), Pos2::ZERO)
        .unwrap();
    let compiler = GraphCompiler::new();
    let streaming = compiler
        .discover_live_capture_feature(widget.graph())
        .unwrap()
        .expect("stream mode should expose a live feature");
    assert_eq!(
        streaming.capabilities().data_delivery(),
        CaptureDataDelivery::DuringAcquisition
    );
    let state = &mut widget.graph_mut().nodes.get_mut(&source).unwrap().state;
    select(state, "mode", "Buffer");
    enable_channels(state, &[0, 2, 9]);

    let feature = compiler
        .discover_live_capture_feature(widget.graph())
        .unwrap()
        .expect("buffer mode should expose the concrete live feature");
    assert_eq!(feature.source_node(), source);
    assert_eq!(
        feature.channels(),
        [
            CaptureChannelId::new("u3pro16:input:0"),
            CaptureChannelId::new("u3pro16:input:2"),
            CaptureChannelId::new("u3pro16:input:9"),
        ]
    );
    assert_eq!(
        feature.capabilities().data_delivery(),
        CaptureDataDelivery::BufferedUpload
    );
    assert!(
        feature
            .capabilities()
            .supports(feature.channels(), feature.sample_rate_hz())
    );

    let edited = compiler
        .apply_live_capture_edit(
            widget.graph(),
            source,
            &LiveCaptureEdit::SetSimpleTrigger {
                channel_id: CaptureChannelId::new("u3pro16:input:2"),
                condition: SimpleTriggerCondition::Falling,
            },
        )
        .unwrap();
    widget.graph_mut().nodes.get_mut(&source).unwrap().state = edited;
    let feature = compiler
        .discover_live_capture_feature(widget.graph())
        .unwrap()
        .unwrap();
    assert_eq!(
        feature.simple_trigger_channels()[1].condition,
        SimpleTriggerCondition::Falling
    );
}

#[test]
fn buffered_hardware_discovery_rejects_too_many_channels_for_the_rate() {
    let mut widget = NodeGraphWidget::new(build_registry());
    let source = widget
        .add_node_at(node_name(U3PRO16_ID), Pos2::ZERO)
        .unwrap();
    let state = &mut widget.graph_mut().nodes.get_mut(&source).unwrap().state;
    select(state, "mode", "Buffer");
    select(state, "sample_rate", "1 GHz");
    let enabled = state["channels"]["enabled"]
        .as_array_mut()
        .expect("capture source channels are an array");
    enabled.fill(serde_json::Value::Bool(true));
    let error = GraphCompiler::new()
        .discover_live_capture_feature(widget.graph())
        .err()
        .expect("wide 1 GHz buffered capture must be rejected before opening hardware");

    assert!(
        error
            .message
            .contains("Too many channels for 1 GHz in Buffer mode"),
        "{}",
        error.message
    );
}

#[test]
fn streaming_hardware_discovery_rejects_too_many_channels_for_the_rate() {
    let mut widget = NodeGraphWidget::new(build_registry());
    let source = widget
        .add_node_at(node_name(U3PRO16_ID), Pos2::ZERO)
        .unwrap();
    let state = &mut widget.graph_mut().nodes.get_mut(&source).unwrap().state;
    select(state, "mode", "Stream");
    select(state, "sample_rate", "1 GHz");
    enable_channels(state, &[0, 3]);
    let error = GraphCompiler::new()
        .discover_live_capture_feature(widget.graph())
        .err()
        .expect("four-input 1 GHz stream must be rejected before opening hardware");

    assert!(
        error
            .message
            .contains("Too many channels for 1 GHz in Stream mode"),
        "{}",
        error.message
    );
}

#[test]
fn dsl_source_presentation_is_builder_owned_after_node_rename() {
    let mut widget = NodeGraphWidget::new(build_registry());
    test_graphs_tests::populate_startup(&mut widget);
    let source_id = *widget
        .graph()
        .nodes
        .iter()
        .find(|(_, node)| node.def_name() == node_name(DSL_FILE_SOURCE_ID))
        .map(|(id, _)| id)
        .unwrap();
    widget.graph_mut().nodes.get_mut(&source_id).unwrap().title = "My capture".to_owned();
    widget.graph_mut().nodes.get_mut(&source_id).unwrap().state["file"]["value"] =
        serde_json::Value::String("capture.dsl".to_owned());
    let presentation = GraphCompiler::new()
        .discover_capture_presentation(widget.graph())
        .unwrap()
        .unwrap();
    let CapturePresentation::Indexed { identity, .. } = presentation.presentation else {
        panic!("DSL source should provide an indexed presentation");
    };
    assert_eq!(identity, std::path::PathBuf::from("capture.dsl"));
}
