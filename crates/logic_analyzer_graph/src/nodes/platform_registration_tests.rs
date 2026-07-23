use egui::Pos2;

use node_graph::{NodeDef, NodeGraphWidget, SocketDirection, SocketId};
use signal_processing::{CaptureChannelId, CaptureDataDelivery, SimpleTriggerCondition};

use super::sources::{DsLogicU3Pro16, U3Pro16State};
use crate::nodes::{DslFileSource, Viewer, build_registry, test_graphs_tests};
use crate::{
    BuilderRegistry, LiveCaptureEdit, apply_live_capture_edit, discover_live_capture_feature, lower,
};

fn attach_viewer_sink(widget: &mut NodeGraphWidget, source: node_graph::NodeId) {
    let viewer = widget
        .add_node_at(Viewer::name(), Pos2::new(320.0, 0.0))
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
        .add_node_at(DsLogicU3Pro16::name(), Pos2::ZERO)
        .expect("native hardware source should be registered");
    attach_viewer_sink(&mut widget, source);

    let compiled = lower(widget.graph(), &BuilderRegistry::standard()).unwrap();
    assert!(
        compiled
            .nodes
            .iter()
            .any(|node| node.builder == DsLogicU3Pro16::name())
    );
}

#[test]
fn buffered_hardware_feature_lowers_opaque_channels_and_portable_trigger_edits() {
    let mut widget = NodeGraphWidget::new(build_registry());
    let source = widget
        .add_node_at(DsLogicU3Pro16::name(), Pos2::ZERO)
        .unwrap();
    let streaming = discover_live_capture_feature(widget.graph(), &BuilderRegistry::standard())
        .unwrap()
        .expect("stream mode should expose a live feature");
    assert_eq!(
        streaming.capabilities().data_delivery(),
        CaptureDataDelivery::DuringAcquisition
    );
    let mut state =
        serde_json::from_value::<U3Pro16State>(widget.graph().nodes[&source].state.clone())
            .unwrap();
    state.mode.select("Buffer");
    state.channels.enabled.fill(false);
    for channel in [0, 2, 9] {
        state.channels.enabled[channel] = true;
    }
    widget.graph_mut().nodes.get_mut(&source).unwrap().state = serde_json::to_value(state).unwrap();

    let builders = BuilderRegistry::standard();
    let feature = discover_live_capture_feature(widget.graph(), &builders)
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

    let edited = apply_live_capture_edit(
        widget.graph(),
        &builders,
        source,
        &LiveCaptureEdit::SetSimpleTrigger {
            channel_id: CaptureChannelId::new("u3pro16:input:2"),
            condition: SimpleTriggerCondition::Falling,
        },
    )
    .unwrap();
    widget.graph_mut().nodes.get_mut(&source).unwrap().state = edited;
    let feature = discover_live_capture_feature(widget.graph(), &builders)
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
        .add_node_at(DsLogicU3Pro16::name(), Pos2::ZERO)
        .unwrap();
    let mut state =
        serde_json::from_value::<U3Pro16State>(widget.graph().nodes[&source].state.clone())
            .unwrap();
    state.mode.select("Buffer");
    state.sample_rate.select("1 GHz");
    state.channels.enabled.fill(true);
    widget.graph_mut().nodes.get_mut(&source).unwrap().state = serde_json::to_value(state).unwrap();
    let error = discover_live_capture_feature(widget.graph(), &BuilderRegistry::standard())
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
        .add_node_at(DsLogicU3Pro16::name(), Pos2::ZERO)
        .unwrap();
    let mut state =
        serde_json::from_value::<U3Pro16State>(widget.graph().nodes[&source].state.clone())
            .unwrap();
    state.mode.select("Stream");
    state.sample_rate.select("1 GHz");
    state.channels.enabled.fill(false);
    state.channels.enabled[0] = true;
    state.channels.enabled[3] = true;
    widget.graph_mut().nodes.get_mut(&source).unwrap().state = serde_json::to_value(state).unwrap();
    let error = discover_live_capture_feature(widget.graph(), &BuilderRegistry::standard())
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
        .find(|(_, node)| node.def_name() == DslFileSource::name())
        .map(|(id, _)| id)
        .unwrap();
    widget.graph_mut().nodes.get_mut(&source_id).unwrap().title = "My capture".to_owned();
    let mut state = serde_json::from_value::<crate::nodes::DslFileSourceState>(
        widget.graph().nodes[&source_id].state.clone(),
    )
    .unwrap();
    state.file.value = "capture.dsl".into();
    widget.graph_mut().nodes.get_mut(&source_id).unwrap().state =
        serde_json::to_value(state).unwrap();
    let presentation =
        crate::discover_capture_presentation(widget.graph(), &BuilderRegistry::standard())
            .unwrap()
            .unwrap();
    let crate::CapturePresentation::Indexed { identity, .. } = presentation.presentation else {
        panic!("DSL source should provide an indexed presentation");
    };
    assert_eq!(identity, std::path::PathBuf::from("capture.dsl"));
}
