use super::{NodeInstance, NodeRuntime, TypedNode};
use crate::api::NodeDef;
use crate::model::{Node, NodeId, NodeKind, Socket};
use egui::Pos2;

// ── Low-level node construction ───────────────────────────────────────────────

fn build_node<T: NodeDef>(id: NodeId, pos: Pos2, state: T::State) -> NodeRuntime {
    let inputs = T::inputs();
    let outputs = T::outputs();
    let properties = T::props();
    let state_json = serde_json::to_value(&state).expect("node state must serialize");
    let input_sockets = inputs
        .iter()
        .map(|input| Socket {
            name: input.label.clone(),
            type_name: input.type_name.to_owned(),
            color: input.color,
            shape: input.shape,
            visible: true,
            hidden: false,
            has_control: input.control.is_some(),
        })
        .collect();
    let output_sockets = outputs
        .iter()
        .map(|output| Socket {
            name: output.label.clone(),
            type_name: output.type_name.to_owned(),
            color: output.color,
            shape: output.shape,
            visible: true,
            hidden: false,
            has_control: output.control.is_some(),
        })
        .collect();
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

    if node.inputs.len() != inputs.len() {
        node.inputs = inputs
            .iter()
            .map(|input| Socket {
                name: input.label.clone(),
                type_name: input.type_name.to_owned(),
                color: input.color,
                shape: input.shape,
                visible: true,
                hidden: false,
                has_control: input.control.is_some(),
            })
            .collect();
    } else {
        for (socket, definition) in node.inputs.iter_mut().zip(&inputs) {
            socket.has_control = definition.control.is_some();
        }
    }
    if node.outputs.len() != outputs.len() {
        node.outputs = outputs
            .iter()
            .map(|output| Socket {
                name: output.label.clone(),
                type_name: output.type_name.to_owned(),
                color: output.color,
                shape: output.shape,
                visible: true,
                hidden: false,
                has_control: output.control.is_some(),
            })
            .collect();
    } else {
        for (socket, definition) in node.outputs.iter_mut().zip(&outputs) {
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

#[derive(Default)]
pub struct NodeTypeRegistry {
    types: Vec<RegisteredNodeType>,
}

impl NodeTypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: NodeDef>(&mut self) -> &mut Self {
        self.types.push(RegisteredNodeType::from_def::<T>());
        self
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
