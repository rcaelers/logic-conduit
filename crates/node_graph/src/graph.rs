use crate::types::{SocketShape, SocketTypeDef};
use crate::value::NodeValue;
use egui::{Color32, Pos2};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub type UpdateFn = fn(&mut [InputSocket], &mut [Socket], &[Prop]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SocketId {
    pub node: NodeId,
    pub index: usize,
    pub is_output: bool,
}

/// An input socket on a node instance. May carry an inline default value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSocket {
    pub name: String,
    pub type_name: String,
    pub color: Color32,
    pub shape: SocketShape,
    pub visible: bool,
    pub value: Option<Box<dyn NodeValue>>,
}

/// An output socket on a node instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Socket {
    pub name: String,
    pub type_name: String,
    pub color: Color32,
    pub shape: SocketShape,
    pub visible: bool,
}

/// A named property on a node instance (not connectable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prop {
    pub id: String,
    pub label: String,
    pub value: Box<dyn NodeValue>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum NodeKind {
    #[default]
    Regular,
    Reroute,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub title: String,
    pub header_color: Color32,
    pub pos: Pos2,
    pub inputs: Vec<InputSocket>,
    pub outputs: Vec<Socket>,
    pub props: Vec<Prop>,
    pub selected: bool,
    #[serde(skip)]
    pub update_fn: Option<UpdateFn>,
}

impl Node {
    pub fn new_reroute(id: NodeId, pos: Pos2) -> Self {
        let any = |name: &str| InputSocket {
            name: name.to_string(),
            type_name: "Any".to_string(),
            color: Color32::from_rgb(150, 150, 150),
            shape: SocketShape::Circle,
            visible: true,
            value: None,
        };
        let out = Socket {
            name: String::new(),
            type_name: "Any".to_string(),
            color: Color32::from_rgb(150, 150, 150),
            shape: SocketShape::Circle,
            visible: true,
        };
        Self {
            id,
            kind: NodeKind::Reroute,
            title: String::new(),
            header_color: Color32::from_rgb(80, 80, 80),
            pos,
            inputs: vec![any("")],
            outputs: vec![out],
            props: vec![],
            selected: false,
            update_fn: None,
        }
    }

    pub fn run_update(&mut self) {
        if let Some(f) = self.update_fn {
            f(&mut self.inputs, &mut self.outputs, &self.props);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub from: SocketId,
    pub to: SocketId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameId(pub u32);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub id: FrameId,
    pub label: String,
    pub color: Color32,
    pub node_ids: Vec<NodeId>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct GraphState {
    pub nodes: HashMap<NodeId, Node>,
    pub connections: Vec<Connection>,
    pub frames: Vec<Frame>,
    next_id: u32,
    next_frame_id: u32,
}

impl GraphState {
    pub fn next_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn add_node(&mut self, node: Node) {
        self.nodes.insert(node.id, node);
    }

    pub fn remove_node(&mut self, id: NodeId) {
        self.nodes.remove(&id);
        self.connections
            .retain(|c| c.from.node != id && c.to.node != id);
    }

    pub fn add_connection(&mut self, from: SocketId, to: SocketId) {
        self.connections.retain(|c| c.to != to);
        self.connections.push(Connection { from, to });
    }

    pub fn is_input_connected(&self, socket: SocketId) -> bool {
        self.connections.iter().any(|c| c.to == socket)
    }

    pub fn sorted_node_ids(&self) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        ids
    }

    pub fn add_frame(&mut self, label: String, color: Color32, node_ids: Vec<NodeId>) -> FrameId {
        let id = FrameId(self.next_frame_id);
        self.next_frame_id += 1;
        self.frames.push(Frame {
            id,
            label,
            color,
            node_ids,
        });
        id
    }

    pub fn cleanup_frames(&mut self) {
        let alive: HashSet<NodeId> = self.nodes.keys().copied().collect();
        for f in &mut self.frames {
            f.node_ids.retain(|id| alive.contains(id));
        }
        self.frames.retain(|f| !f.node_ids.is_empty());
    }
}

// ── Node definition API ───────────────────────────────────────────────────────

pub struct InputDef {
    pub label: &'static str,
    pub type_name: &'static str,
    pub color: Color32,
    pub shape: SocketShape,
    pub value: Option<Box<dyn NodeValue>>,
}

impl InputDef {
    pub fn new<T: SocketTypeDef>(label: &'static str) -> Self {
        Self {
            label,
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
            value: None,
        }
    }

    pub fn with_value<T: SocketTypeDef>(label: &'static str, value: Box<dyn NodeValue>) -> Self {
        Self {
            label,
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
            value: Some(value),
        }
    }
}

pub struct OutputDef {
    pub label: &'static str,
    pub type_name: &'static str,
    pub color: Color32,
    pub shape: SocketShape,
}

impl OutputDef {
    pub fn new<T: SocketTypeDef>(label: &'static str) -> Self {
        Self {
            label,
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
        }
    }
}

pub struct PropDef {
    pub id: &'static str,
    pub label: &'static str,
    pub value: Box<dyn NodeValue>,
}

impl PropDef {
    pub fn new(id: &'static str, label: &'static str, value: Box<dyn NodeValue>) -> Self {
        Self { id, label, value }
    }
}

pub trait NodeDef {
    fn name() -> &'static str
    where
        Self: Sized;
    fn category() -> &'static str
    where
        Self: Sized;
    fn color() -> Color32
    where
        Self: Sized,
    {
        Color32::from_rgb(80, 80, 80)
    }
    fn inputs() -> Vec<InputDef>
    where
        Self: Sized;
    fn outputs() -> Vec<OutputDef>
    where
        Self: Sized;
    fn props() -> Vec<PropDef>
    where
        Self: Sized,
    {
        vec![]
    }
    fn on_update() -> Option<UpdateFn>
    where
        Self: Sized,
    {
        None
    }
}

// ── Internal class definition (used by NodeTypeRegistry) ─────────────────────

pub(crate) struct SocketDecl {
    pub name: String,
    pub type_name: String,
    pub color: Color32,
    pub shape: SocketShape,
    pub value: Option<Box<dyn NodeValue>>,
}

pub(crate) struct OutputDecl {
    pub name: String,
    pub type_name: String,
    pub color: Color32,
    pub shape: SocketShape,
}

pub(crate) struct PropDecl {
    pub id: String,
    pub label: String,
    pub value: Box<dyn NodeValue>,
}

pub(crate) struct NodeClassDef {
    pub name: String,
    pub category: String,
    pub header_color: Color32,
    pub inputs: Vec<SocketDecl>,
    pub outputs: Vec<OutputDecl>,
    pub props: Vec<PropDecl>,
    pub update_fn: Option<UpdateFn>,
}

impl NodeClassDef {
    pub fn from_def<T: NodeDef>() -> Self {
        let inputs = T::inputs()
            .into_iter()
            .map(|d| SocketDecl {
                name: d.label.to_owned(),
                type_name: d.type_name.to_owned(),
                color: d.color,
                shape: d.shape,
                value: d.value,
            })
            .collect();

        let outputs = T::outputs()
            .into_iter()
            .map(|d| OutputDecl {
                name: d.label.to_owned(),
                type_name: d.type_name.to_owned(),
                color: d.color,
                shape: d.shape,
            })
            .collect();

        let props = T::props()
            .into_iter()
            .map(|p| PropDecl {
                id: p.id.to_owned(),
                label: p.label.to_owned(),
                value: p.value,
            })
            .collect();

        Self {
            name: T::name().to_owned(),
            category: T::category().to_owned(),
            header_color: T::color(),
            inputs,
            outputs,
            props,
            update_fn: T::on_update(),
        }
    }
}
