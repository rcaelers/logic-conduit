use super::{NodeInstance, NodeRuntime, TypedNode};
use crate::api::{InputDef, NodeDef, OutputDef};
use crate::model::{Node, NodeId, NodeKind, Socket, SocketShape, VariadicInfo};
use egui::{Color32, Pos2};
use std::collections::HashMap;

// ── Low-level node construction ───────────────────────────────────────────────

fn input_socket<S>(def_index: usize, input: &InputDef<S>) -> Socket {
    Socket {
        name: input.label.clone(),
        type_name: input.type_name.to_owned(),
        color: input.color,
        shape: input.shape,
        allowed: input
            .accepted
            .iter()
            .map(|identity| identity.name.to_owned())
            .collect(),
        resolved_type: None,
        def_index,
        variadic: input.variadic_max.map(|max| VariadicInfo {
            base: input.label.clone(),
            max,
            placeholder: true,
        }),
        visible: true,
        hidden: false,
        has_control: input.control.is_some() && input.variadic_max.is_none(),
    }
}

fn output_socket<S>(def_index: usize, output: &OutputDef<S>) -> Socket {
    Socket {
        name: output.label.clone(),
        type_name: output.type_name.to_owned(),
        color: output.color,
        shape: output.shape,
        allowed: Vec::new(),
        resolved_type: None,
        def_index,
        variadic: None,
        visible: true,
        hidden: false,
        has_control: output.control.is_some(),
    }
}

fn build_input_sockets<S>(inputs: &[InputDef<S>]) -> Vec<Socket> {
    inputs
        .iter()
        .enumerate()
        .map(|(index, input)| input_socket(index, input))
        .collect()
}

fn build_output_sockets<S>(outputs: &[OutputDef<S>]) -> Vec<Socket> {
    outputs
        .iter()
        .enumerate()
        .map(|(index, output)| output_socket(index, output))
        .collect()
}

/// Brings restored input sockets in line with the current defs. Socket and
/// def counts legitimately diverge for variadic groups, so sockets are
/// validated structurally against the defs (via `def_index`); a match keeps
/// them as saved with per-def data refreshed, anything else rebuilds from the
/// defs (matching the old count-mismatch behavior).
fn reconcile_input_sockets<S>(sockets: &mut Vec<Socket>, defs: &[InputDef<S>]) {
    // Files saved before `def_index` existed default it to 0 everywhere;
    // upgrade positionally when the layout is plainly pre-variadic.
    if sockets.len() == defs.len()
        && sockets
            .iter()
            .all(|socket| socket.def_index == 0 && socket.variadic.is_none())
    {
        for (index, socket) in sockets.iter_mut().enumerate() {
            socket.def_index = index;
        }
    }

    if !input_sockets_match_defs(sockets, defs) {
        *sockets = build_input_sockets(defs);
        return;
    }

    for socket in sockets.iter_mut() {
        let definition = &defs[socket.def_index];
        socket.has_control = definition.control.is_some() && definition.variadic_max.is_none();
        socket.allowed = definition
            .accepted
            .iter()
            .map(|identity| identity.name.to_owned())
            .collect();
        if let Some(resolved) = socket.resolved_type.clone()
            && !socket.accepts(&resolved)
        {
            socket.resolved_type = None;
        }
        if let Some(info) = &mut socket.variadic {
            info.base = definition.label.clone();
            if let Some(max) = definition.variadic_max {
                info.max = max;
            }
        }
    }
}

fn input_sockets_match_defs<S>(sockets: &[Socket], defs: &[InputDef<S>]) -> bool {
    let mut iter = sockets.iter().peekable();
    for (def_index, definition) in defs.iter().enumerate() {
        match definition.variadic_max {
            None => {
                let Some(socket) = iter.next() else {
                    return false;
                };
                if socket.def_index != def_index || socket.variadic.is_some() {
                    return false;
                }
            }
            Some(max) => {
                let mut members = 0usize;
                let mut placeholders = 0usize;
                while iter.peek().is_some_and(|socket| socket.def_index == def_index) {
                    let socket = iter.next().expect("peeked");
                    match &socket.variadic {
                        Some(info) if info.placeholder => placeholders += 1,
                        Some(_) => members += 1,
                        None => return false,
                    }
                }
                if members > max || placeholders > 1 {
                    return false;
                }
                if placeholders == 0 && members < max {
                    return false;
                }
            }
        }
    }
    iter.next().is_none()
}

fn build_node<T: NodeDef>(id: NodeId, pos: Pos2, state: T::State) -> NodeRuntime {
    let inputs = T::inputs();
    let outputs = T::outputs();
    let properties = T::props();
    let state_json = serde_json::to_value(&state).expect("node state must serialize");
    let input_sockets = build_input_sockets(&inputs);
    let output_sockets = build_output_sockets(&outputs);
    let mut node = Node {
        id,
        kind: NodeKind::Regular,
        title: T::name().to_owned(),
        header_color: T::color(),
        pos,
        inputs: input_sockets,
        outputs: output_sockets,
        collapsed: false,
        state: state_json,
        property_count: properties.len(),
        selected: false,
    };
    let mut instance: Box<dyn NodeInstance> = Box::new(TypedNode::<T> {
        state,
        inputs,
        outputs,
        properties,
    });
    instance.update(&mut node.inputs, &mut node.outputs);
    node.state = instance.save_state();
    NodeRuntime { node, instance }
}

pub(crate) fn create_node<T: NodeDef>(id: NodeId, pos: Pos2) -> NodeRuntime {
    build_node::<T>(id, pos, T::state())
}

pub(crate) fn restore_node<T: NodeDef>(node: &mut Node) -> Box<dyn NodeInstance> {
    let state = serde_json::from_value(node.state.clone()).unwrap_or_else(|_| T::state());
    let inputs = T::inputs();
    let outputs = T::outputs();
    let properties = T::props();

    reconcile_input_sockets(&mut node.inputs, &inputs);
    if node.outputs.len() != outputs.len() {
        node.outputs = build_output_sockets(&outputs);
    } else {
        for (index, (socket, definition)) in node.outputs.iter_mut().zip(&outputs).enumerate() {
            socket.def_index = index;
            socket.has_control = definition.control.is_some();
        }
    }

    node.property_count = properties.len();
    let mut instance: Box<dyn NodeInstance> = Box::new(TypedNode::<T> {
        state,
        inputs,
        outputs,
        properties,
    });
    instance.update(&mut node.inputs, &mut node.outputs);
    node.state = instance.save_state();
    instance
}

// ── RegisteredNodeType ────────────────────────────────────────────────────────

pub(crate) struct RegisteredNodeType {
    pub name: String,
    pub category: String,
    pub create: fn(NodeId, Pos2) -> NodeRuntime,
    pub restore: fn(&mut Node) -> Box<dyn NodeInstance>,
}

impl RegisteredNodeType {
    pub fn from_def<T: NodeDef>() -> Self {
        Self {
            name: T::name().to_owned(),
            category: T::category().to_owned(),
            create: create_node::<T>,
            restore: restore_node::<T>,
        }
    }
}

// ── NodeTypeRegistry ──────────────────────────────────────────────────────────

/// Graph-wide identity of a socket type, collected from node defs as they
/// register. Used to re-skin sockets that resolved to another accepted type;
/// idle looks stay under node-def control.
#[derive(Debug, Clone, Copy)]
pub struct SocketTypeStyle {
    pub color: Color32,
    pub shape: SocketShape,
}

#[derive(Default)]
pub struct NodeTypeRegistry {
    types: Vec<RegisteredNodeType>,
    socket_types: HashMap<String, SocketTypeStyle>,
}

impl NodeTypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: NodeDef>(&mut self) -> &mut Self {
        for input in T::inputs() {
            self.record_socket_type(&input.identity);
            for accepted in &input.accepted {
                self.record_socket_type(accepted);
            }
        }
        for output in T::outputs() {
            self.record_socket_type(&output.identity);
        }
        self.types.push(RegisteredNodeType::from_def::<T>());
        self
    }

    fn record_socket_type(&mut self, identity: &crate::api::SocketTypeIdentity) {
        self.socket_types
            .entry(identity.name.to_owned())
            .or_insert(SocketTypeStyle {
                color: identity.color,
                shape: identity.shape,
            });
    }

    pub fn socket_type_style(&self, type_name: &str) -> Option<SocketTypeStyle> {
        self.socket_types.get(type_name).copied()
    }

    /// The look a socket should be drawn with right now: the connected type's
    /// graph-wide identity while resolved, the socket's own (idle) look
    /// otherwise.
    pub(crate) fn socket_display(&self, socket: &Socket) -> (Color32, SocketShape) {
        socket
            .resolved_type
            .as_deref()
            .and_then(|resolved| self.socket_type_style(resolved))
            .map(|style| (style.color, style.shape))
            .unwrap_or((socket.color, socket.shape))
    }

    pub(crate) fn all(&self) -> &[RegisteredNodeType] {
        &self.types
    }

    pub(crate) fn find(&self, name: &str) -> Option<&RegisteredNodeType> {
        self.types.iter().find(|d| d.name == name)
    }

    pub(crate) fn instantiate(&self, name: &str, id: NodeId, pos: Pos2) -> Option<NodeRuntime> {
        let def = self.find(name)?;
        Some((def.create)(id, pos))
    }

    pub(crate) fn restore_node(&self, node: &mut Node) -> Option<Box<dyn NodeInstance>> {
        if node.kind == NodeKind::Regular
            && let Some(definition) = self.find(&node.title)
        {
            return Some((definition.restore)(node));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AnySocket, FloatSocket, InputDef, IntSocket, NodeDef, OutputDef};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct MixState;

    struct MixNode;
    impl NodeDef for MixNode {
        type State = MixState;

        fn name() -> &'static str {
            "Mix"
        }
        fn category() -> &'static str {
            "Test"
        }
        fn inputs() -> Vec<InputDef<MixState>> {
            vec![
                InputDef::new::<FloatSocket>("Gain").accepts::<IntSocket>(),
                InputDef::new::<AnySocket>("In").variadic(3),
            ]
        }
        fn outputs() -> Vec<OutputDef<MixState>> {
            vec![OutputDef::new::<FloatSocket>("Out")]
        }
        fn state() -> MixState {
            MixState
        }
    }

    #[test]
    fn create_builds_placeholder_and_allowed() {
        let runtime = create_node::<MixNode>(NodeId(0), Pos2::ZERO);
        let inputs = &runtime.node.inputs;
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].allowed, vec!["Int".to_owned()]);
        assert_eq!(inputs[0].def_index, 0);
        assert!(inputs[1].is_variadic_placeholder());
        assert_eq!(inputs[1].def_index, 1);
    }

    #[test]
    fn restore_keeps_grown_variadic_members() {
        let runtime = create_node::<MixNode>(NodeId(0), Pos2::ZERO);
        let mut node = runtime.node;
        // Simulate a grown group as add_connection would leave it.
        let placeholder = node.inputs[1].clone();
        let mut member = placeholder.clone();
        member.name = "In 1".to_owned();
        if let Some(info) = &mut member.variadic {
            info.placeholder = false;
        }
        node.inputs[1] = member;
        node.inputs.push(placeholder);

        restore_node::<MixNode>(&mut node);
        assert_eq!(node.inputs.len(), 3);
        assert!(node.inputs[1].is_variadic_member());
        assert_eq!(node.inputs[1].name, "In 1");
        assert!(node.inputs[2].is_variadic_placeholder());
    }

    #[test]
    fn restore_upgrades_legacy_def_indices() {
        let runtime = create_node::<MixNode>(NodeId(0), Pos2::ZERO);
        let mut node = runtime.node;
        // Legacy files: def_index defaulted to 0 everywhere, no variadic info.
        for socket in &mut node.inputs {
            socket.def_index = 0;
            socket.variadic = None;
        }

        restore_node::<MixNode>(&mut node);
        // Structure no longer matches (variadic def has a plain socket), so
        // sockets are rebuilt from the defs.
        assert_eq!(node.inputs.len(), 2);
        assert!(node.inputs[1].is_variadic_placeholder());
        assert_eq!(node.inputs[0].def_index, 0);
        assert_eq!(node.inputs[1].def_index, 1);
    }

    #[test]
    fn restore_rebuilds_when_defs_changed() {
        let runtime = create_node::<MixNode>(NodeId(0), Pos2::ZERO);
        let mut node = runtime.node;
        node.inputs.remove(0);

        restore_node::<MixNode>(&mut node);
        assert_eq!(node.inputs.len(), 2);
        assert_eq!(node.inputs[0].type_name, "Float");
        assert!(node.inputs[1].is_variadic_placeholder());
    }
}
