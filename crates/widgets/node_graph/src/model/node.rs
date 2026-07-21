use egui::{Color32, Pos2};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ids::NodeId;
use super::socket::{Socket, SocketShape};

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
    /// Bypassed for compilation (Phase 3): the compiler splices its
    /// compatible inputs directly to its outputs and drops the node, rather
    /// than building it. Non-destructive — the node, its config, and its
    /// wires all stay in the graph; toggling again restores it.
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub state: Value,
    #[serde(flatten)]
    pub metadata: NodeMetadata,
    /// Def-driven status message, recomputed on every state update.
    #[serde(skip)]
    pub badge: Option<NodeBadge>,
    pub selected: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NodeMetadata {
    #[serde(skip)]
    property_count: usize,
}

impl NodeMetadata {
    pub(crate) fn with_property_count(property_count: usize) -> Self {
        Self { property_count }
    }
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
            muted: self.muted,
            state: self.state.clone(),
            metadata: self.metadata.clone(),
            badge: self.badge.clone(),
            selected: self.selected,
        }
    }
}

impl Node {
    pub(crate) fn property_count(&self) -> usize {
        self.metadata.property_count
    }

    pub(crate) fn set_property_count(&mut self, property_count: usize) {
        self.metadata.property_count = property_count;
    }

    /// The registered node-type name this node was created from.
    pub fn def_name(&self) -> &str {
        if self.type_name.is_empty() {
            &self.title
        } else {
            &self.type_name
        }
    }

    /// The input↔output pairing a muted node bypasses through: for each
    /// output (in order), the earliest not-yet-claimed input whose type is
    /// compatible with it. Purely a function of this node's own declared
    /// sockets — independent of whatever happens to be wired upstream or
    /// downstream. Mirrors Blender: muting only usefully bypasses a node
    /// whose input and output share a type (e.g. `Buffer`'s `Any`/`Any`); a
    /// type-transforming node (Signal → Words) has no such pair, so muting
    /// it drops its output rather than faking one.
    pub fn mute_pass_through_pairs(&self) -> Vec<(usize, usize)> {
        let mut used = vec![false; self.inputs.len()];
        let mut pairs = Vec::new();
        for (out_idx, output) in self.outputs.iter().enumerate() {
            let Some(in_idx) = self
                .inputs
                .iter()
                .enumerate()
                .position(|(i, input)| !used[i] && input.accepts(output.effective_type()))
            else {
                continue;
            };
            used[in_idx] = true;
            pairs.push((out_idx, in_idx));
        }
        pairs
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
            editor_visible: true,
            hidden: false,
            has_control: false,
            view_selectable: false,
            view_indicator_sources: Vec::new(),
            show_in_view: false,
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
            muted: false,
            state: Value::Null,
            metadata: NodeMetadata::default(),
            badge: None,
            selected: false,
        }
    }

    /// A regular node with no properties panel, its sockets and state left
    /// for the caller to fill in. For building a `Node` directly outside the
    /// widget/registry path — e.g. the compiler's synthetic auto-view sink,
    /// which is never rendered and so has no properties panel to size.
    pub fn blank(id: NodeId, type_name: impl Into<String>, pos: Pos2) -> Self {
        let type_name = type_name.into();
        Self {
            id,
            kind: NodeKind::Regular,
            title: type_name.clone(),
            type_name,
            header_color: Color32::from_rgb(80, 80, 80),
            pos,
            inputs: Vec::new(),
            outputs: Vec::new(),
            collapsed: false,
            muted: false,
            state: Value::Null,
            metadata: NodeMetadata::default(),
            badge: None,
            selected: false,
        }
    }
}
