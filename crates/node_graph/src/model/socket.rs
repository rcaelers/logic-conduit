use egui::Color32;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SocketShape {
    #[default]
    Circle,
    Diamond,
    Square,
    Triangle,
}

/// State of a socket that belongs to a growing input group. A `placeholder`
/// waits for a connection; connecting converts it to a member and spawns a
/// fresh placeholder (until `max` members exist). Disconnecting a member
/// removes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariadicInfo {
    /// Group label from the def; members display as "{base} {n}".
    pub base: String,
    /// Maximum number of members.
    pub max: usize,
    pub placeholder: bool,
}

/// A socket on a node instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Socket {
    pub name: String,
    /// Native type. For inputs this is what the node primarily expects; the
    /// socket may temporarily resolve to one of `allowed` while connected.
    pub type_name: String,
    /// Idle look, owned by the node definition (`idle_style` / `on_update`).
    /// The resolved look is derived from the type identity table at render time.
    pub color: Color32,
    pub shape: SocketShape,
    /// Additional type names this input accepts besides `type_name`. The node
    /// declared it can handle these itself. Empty = strict.
    #[serde(default)]
    pub allowed: Vec<String>,
    /// Set while connected to an output whose type differs from `type_name`.
    #[serde(default)]
    pub resolved_type: Option<String>,
    /// Which `InputDef`/`OutputDef` of the node definition this socket came
    /// from. Socket and def counts diverge once variadic groups grow, so all
    /// def lookups (controls, restore) go through this instead of position.
    #[serde(default)]
    pub def_index: usize,
    /// Present when this socket belongs to a variadic group.
    #[serde(default)]
    pub variadic: Option<VariadicInfo>,
    /// Controlled by `on_update` — set false to suppress the socket entirely.
    pub visible: bool,
    /// Set true by the user via "Hide Unused"; never touched by `on_update`.
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub has_control: bool,
}

impl Socket {
    /// The type this socket currently carries: the connected type while
    /// resolved, the native type otherwise.
    pub fn effective_type(&self) -> &str {
        self.resolved_type.as_deref().unwrap_or(&self.type_name)
    }

    /// Whether this input socket accepts a connection from an output of
    /// `incoming` type. Acceptance is per-socket (declared by the node),
    /// not a property of the socket type.
    pub fn accepts(&self, incoming: &str) -> bool {
        incoming == "Any"
            || self.type_name == "Any"
            || incoming == self.type_name
            || self.allowed.iter().any(|t| t == incoming)
    }

    pub fn is_variadic_placeholder(&self) -> bool {
        self.variadic.as_ref().is_some_and(|info| info.placeholder)
    }

    pub fn is_variadic_member(&self) -> bool {
        self.variadic.as_ref().is_some_and(|info| !info.placeholder)
    }
}
