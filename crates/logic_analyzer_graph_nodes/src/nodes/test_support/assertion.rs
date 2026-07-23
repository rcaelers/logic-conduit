use std::collections::HashSet;

use egui::Pos2;

use logic_analyzer_graph::host::{BuilderRegistry, lower};
use logic_analyzer_graph_api::node::GraphNodeRegistration;
use logic_analyzer_graph_api::node_support::PortKind;
use node_graph::{NodeDef, NodeGraphWidget, NodeTypeRegistry, SocketDirection, SocketId};

use super::endpoints::{self, TestSink, TestSource};

pub(crate) fn assert_node_registration_isolated(stable_id: &str) {
    assert_node_registration_isolated_with_state(stable_id, None);
}

pub(crate) fn assert_node_registration_isolated_with_state(
    stable_id: &str,
    state: Option<serde_json::Value>,
) {
    let registration = inventory::iter::<GraphNodeRegistration>
        .into_iter()
        .find(|registration| registration.stable_id() == stable_id)
        .unwrap_or_else(|| panic!("missing graph-node registration '{stable_id}'"));

    let mut node_types = NodeTypeRegistry::new();
    registration.apply_node(&mut node_types);
    node_types.register::<TestSource>();
    node_types.register::<TestSink>();

    let mut widget = NodeGraphWidget::new(node_types);
    let target = widget
        .add_node_at(registration.name(), Pos2::new(300.0, 100.0))
        .unwrap_or_else(|| panic!("isolated registry did not create '{}'", registration.name()));

    if let Some(state) = state {
        widget.set_node_state(target, state);
    }

    let state = widget.graph().nodes[&target].state.clone();
    let Some(builder) = registration.builder() else {
        return;
    };

    let target_inputs = widget.graph().nodes[&target].inputs.clone();
    let target_outputs = widget.graph().nodes[&target].outputs.clone();
    let mut kinds = HashSet::<PortKind>::new();
    let mut required_inputs = Vec::new();
    for (index, socket) in target_inputs.iter().enumerate() {
        if !socket.visible || !builder.input_required(socket, &state) {
            continue;
        }
        let accepted = builder.accepted_kinds(socket, &state);
        assert!(
            !accepted.is_empty(),
            "{}.{} is required but accepts no runtime payload",
            registration.name(),
            socket.name
        );
        kinds.extend(accepted);
        required_inputs.push(index);
    }

    let mut offered_outputs = Vec::new();
    for (index, socket) in target_outputs.iter().enumerate() {
        if !socket.visible {
            continue;
        }
        let offered = builder.offered_kinds(socket, &state);
        assert!(
            !offered.is_empty(),
            "{}.{} is visible but offers no runtime payload",
            registration.name(),
            socket.name
        );
        kinds.extend(offered);
        offered_outputs.push(index);
    }

    let mut builders = BuilderRegistry::isolated_test();
    if builder.is_data_subscription() {
        kinds.extend(builders.subscribable_payload_kinds());
        if required_inputs.is_empty() {
            let input = target_inputs
                .iter()
                .position(|socket| socket.visible)
                .expect("data subscription exposes an input");
            required_inputs.push(input);
        }
    }
    let kinds = kinds.into_iter().collect::<Vec<_>>();
    endpoints::install_builders(&mut builders, kinds);

    let is_source = builder.is_source();
    let is_sink = builder.is_sink();
    let is_data_subscription = builder.is_data_subscription();
    builders.insert_test_builder(registration.name(), builder);

    if !is_source {
        assert!(
            !required_inputs.is_empty(),
            "non-source '{}' has no required input for isolated lowering",
            registration.name()
        );
        let source = widget
            .add_node_at(TestSource::name(), Pos2::new(0.0, 100.0))
            .expect("test source definition is registered");
        for input in required_inputs {
            connect(
                &mut widget,
                socket_id(source, 0, SocketDirection::Output),
                socket_id(target, input, SocketDirection::Input),
            );
        }
    }

    if !is_sink && !is_data_subscription {
        assert!(
            !offered_outputs.is_empty(),
            "non-sink '{}' has no output for isolated lowering",
            registration.name()
        );
        let sink = widget
            .add_node_at(TestSink::name(), Pos2::new(600.0, 100.0))
            .expect("test sink definition is registered");
        for output in offered_outputs {
            let input = widget.graph().nodes[&sink]
                .inputs
                .iter()
                .position(|socket| socket.visible && socket.variadic.is_some())
                .expect("test sink keeps a variadic placeholder");
            connect(
                &mut widget,
                socket_id(target, output, SocketDirection::Output),
                socket_id(sink, input, SocketDirection::Input),
            );
        }
    }

    let compiled = lower(widget.graph(), &builders).unwrap_or_else(|errors| {
        panic!(
            "isolated lowering of '{}' failed: {errors:#?}",
            registration.name()
        )
    });
    assert!(
        compiled.nodes.iter().any(|node| node.id == target),
        "isolated lowering pruned the target node"
    );
}

fn connect(widget: &mut NodeGraphWidget, from: SocketId, to: SocketId) {
    assert_eq!(from.direction, SocketDirection::Output);
    assert_eq!(to.direction, SocketDirection::Input);
    widget.graph_mut().add_connection(from, to);
}

fn socket_id(node: node_graph::NodeId, index: usize, direction: SocketDirection) -> SocketId {
    SocketId {
        node,
        index,
        direction,
    }
}
