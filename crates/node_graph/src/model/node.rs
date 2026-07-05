use super::{NodeId, Socket, SocketShape};
use egui::{Color32, Pos2};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum NodeKind {
    #[default]
    Regular,
    Reroute,
}

/// Per-node status message rendered under the node body: def-driven
/// validation notes (a clamped setting, an invalid pattern) or externally
/// set compile/runtime errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeBadge {
    pub text: String,
    pub severity: BadgeSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BadgeSeverity {
    Info,
    Warning,
    Error,
}

impl NodeBadge {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: BadgeSeverity::Info,
        }
    }
    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: BadgeSeverity::Warning,
        }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: BadgeSeverity::Error,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    /// Display name; user-renamable. The registered def is identified by
    /// `type_name`, never by the title.
    pub title: String,
    /// Registered node-type name. Empty in files saved before renaming
    /// existed; those fall back to `title` (which then still equals it).
    #[serde(default)]
    pub type_name: String,
    pub header_color: Color32,
    pub pos: Pos2,
    pub inputs: Vec<Socket>,
    pub outputs: Vec<Socket>,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub state: Value,
    #[serde(skip)]
    pub(crate) property_count: usize,
    /// Def-driven status message, recomputed on every state update.
    #[serde(skip)]
    pub badge: Option<NodeBadge>,
    pub selected: bool,
}

impl Clone for Node {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            kind: self.kind.clone(),
            title: self.title.clone(),
            type_name: self.type_name.clone(),
            header_color: self.header_color,
            pos: self.pos,
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
            collapsed: self.collapsed,
            state: self.state.clone(),
            property_count: self.property_count,
            badge: self.badge.clone(),
            selected: self.selected,
        }
    }
}

impl Node {
    /// The registered node-type name this node was created from.
    pub fn def_name(&self) -> &str {
        if self.type_name.is_empty() {
            &self.title
        } else {
            &self.type_name
        }
    }
}

impl Node {
    pub fn new_reroute(id: NodeId, pos: Pos2) -> Self {
        let input = Socket {
            name: String::new(),
            type_name: "Any".to_string(),
            color: Color32::from_rgb(150, 150, 150),
            shape: SocketShape::Circle,
            allowed: Vec::new(),
            resolved_type: None,
            def_index: 0,
            variadic: None,
            visible: true,
            hidden: false,
            has_control: false,
        };
        let output = input.clone();
        Self {
            id,
            kind: NodeKind::Reroute,
            title: String::new(),
            type_name: String::new(),
            header_color: Color32::from_rgb(80, 80, 80),
            pos,
            inputs: vec![input],
            outputs: vec![output],
            collapsed: false,
            state: Value::Null,
            property_count: 0,
            badge: None,
            selected: false,
        }
    }
}
