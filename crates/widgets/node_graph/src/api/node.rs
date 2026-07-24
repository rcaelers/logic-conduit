use std::fmt;
use std::marker::PhantomData;

use egui::{Color32, Rect, Ui};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::control::InlineControl;
use super::socket::{SocketDef, SocketWithControlDef};
use crate::model::{NodeBadge, Socket, SocketShape};

/// Identity of a socket type as it should appear graph-wide: used to re-skin
/// sockets that resolved to this type, regardless of any per-def idle styling.
#[derive(Debug, Clone)]
pub struct SocketTypeIdentity {
    pub name: &'static str,
    pub color: Color32,
    pub shape: SocketShape,
}

impl SocketTypeIdentity {
    fn of<T: SocketDef>() -> Self {
        Self {
            name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
        }
    }
}

pub struct InputDef<S> {
    pub(crate) stable_id: String,
    pub(crate) label: String,
    pub(crate) type_name: &'static str,
    /// Idle look shown while unconnected; defaults to the native type's
    /// identity, overridable per def via [`InputDef::idle_style`].
    pub(crate) color: Color32,
    pub(crate) shape: SocketShape,
    /// Native type identity (never restyled) — feeds the type identity table.
    pub(crate) identity: SocketTypeIdentity,
    /// Extra types this input accepts; the node handles them itself.
    pub(crate) accepted: Vec<SocketTypeIdentity>,
    /// `Some(max)` turns this def into a growing group: it starts as a single
    /// placeholder socket; each connection converts the placeholder into a
    /// member and spawns a new one, up to `max` members.
    pub(crate) variadic_max: Option<usize>,
    pub(crate) control: Option<Box<dyn ControlBinding<S>>>,
}

impl<S: 'static> InputDef<S> {
    pub fn new<T: SocketDef>(label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            stable_id: label.clone(),
            label,
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
            identity: SocketTypeIdentity::of::<T>(),
            accepted: Vec::new(),
            variadic_max: None,
            control: None,
        }
    }

    pub fn control<T: SocketWithControlDef>(
        label: impl Into<String>,
        accessor: for<'a> fn(&'a mut S) -> &'a mut T::Control,
    ) -> Self {
        let label = label.into();
        Self {
            stable_id: label.clone(),
            label: label.clone(),
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
            identity: SocketTypeIdentity::of::<T>(),
            accepted: Vec::new(),
            variadic_max: None,
            control: Some(Box::new(ControlBindingRenderer { label, accessor })),
        }
    }

    /// Sets the persisted schema identity independently of the display label.
    pub fn stable_id(mut self, stable_id: impl Into<String>) -> Self {
        self.stable_id = stable_id.into();
        self
    }

    /// Declares that this input also accepts `T` — the node's processing is
    /// able to handle a `T` on this input (e.g. a constant on a stream input).
    pub fn accepts<T: SocketDef>(mut self) -> Self {
        self.accepted.push(SocketTypeIdentity::of::<T>());
        self
    }

    /// Overrides the look shown while the socket is unconnected. The resolved
    /// look always comes from the connected type's identity.
    pub fn idle_style(mut self, color: Color32, shape: SocketShape) -> Self {
        self.color = color;
        self.shape = shape;
        self
    }

    /// Turns this input into a growing group of up to `max` sockets.
    /// Connecting to the trailing placeholder adds a member ("{label} 1",
    /// "{label} 2", …) and a new placeholder; disconnecting a member removes
    /// it. Variadic inputs cannot carry inline controls.
    pub fn variadic(mut self, max: usize) -> Self {
        self.variadic_max = Some(max.max(1));
        self
    }
}

pub struct OutputDef<S> {
    pub(crate) stable_id: String,
    pub(crate) label: String,
    pub(crate) type_name: &'static str,
    pub(crate) color: Color32,
    pub(crate) shape: SocketShape,
    pub(crate) identity: SocketTypeIdentity,
    pub(crate) control: Option<Box<dyn ControlBinding<S>>>,
    pub(crate) view_selectable: bool,
    pub(crate) editor_visible: bool,
    pub(crate) view_indicator_sources: Vec<usize>,
}

impl<S: 'static> OutputDef<S> {
    pub fn new<T: SocketDef>(label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            stable_id: label.clone(),
            label,
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
            identity: SocketTypeIdentity::of::<T>(),
            control: None,
            view_selectable: true,
            editor_visible: true,
            view_indicator_sources: Vec::new(),
        }
    }

    pub fn control<T: SocketWithControlDef>(
        label: impl Into<String>,
        accessor: for<'a> fn(&'a mut S) -> &'a mut T::Control,
    ) -> Self {
        let label = label.into();
        Self {
            stable_id: label.clone(),
            label: label.clone(),
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
            identity: SocketTypeIdentity::of::<T>(),
            control: Some(Box::new(ControlBindingRenderer { label, accessor })),
            view_selectable: true,
            editor_visible: true,
            view_indicator_sources: Vec::new(),
        }
    }

    /// Sets the persisted schema identity independently of the display label.
    pub fn stable_id(mut self, stable_id: impl Into<String>) -> Self {
        self.stable_id = stable_id.into();
        self
    }

    /// Controls whether this output appears in the generic View panel's lane
    /// selector. Disable this when the host presents the output automatically
    /// through another explicit contract.
    pub fn view_selectable(mut self, selectable: bool) -> Self {
        self.view_selectable = selectable;
        self
    }

    /// Controls whether this output has a socket row in the node editor.
    /// Connected outputs remain visible so existing wires stay editable.
    pub fn editor_visible(mut self, visible: bool) -> Self {
        self.editor_visible = visible;
        self
    }

    /// Makes this output's viewer eye summarize the selected viewer state of
    /// other outputs. Indices refer to this node's output definitions.
    pub fn view_indicator_sources(mut self, sources: impl IntoIterator<Item = usize>) -> Self {
        self.view_indicator_sources = sources.into_iter().collect();
        self
    }
}

type ControlAccessor<S, T> = for<'a> fn(&'a mut S) -> &'a mut T;

pub(crate) trait ControlBinding<S> {
    fn draw(&self, state: &mut S, ui: &mut Ui, rect: Rect, zoom: f32, clip_rect: Rect) -> bool;
}

struct ControlBindingRenderer<S, T> {
    label: String,
    accessor: ControlAccessor<S, T>,
}

impl<S, T: InlineControl> ControlBinding<S> for ControlBindingRenderer<S, T> {
    fn draw(&self, state: &mut S, ui: &mut Ui, rect: Rect, zoom: f32, clip_rect: Rect) -> bool {
        (self.accessor)(state).draw_widget(ui, &self.label, rect, zoom, clip_rect)
    }
}

struct InstanceControlBindingRenderer<S, T, F> {
    label: String,
    accessor: F,
    marker: PhantomData<fn(&mut S) -> &mut T>,
}

impl<S, T, F> ControlBinding<S> for InstanceControlBindingRenderer<S, T, F>
where
    T: InlineControl,
    F: for<'a> Fn(&'a mut S) -> &'a mut T,
{
    fn draw(&self, state: &mut S, ui: &mut Ui, rect: Rect, zoom: f32, clip_rect: Rect) -> bool {
        (self.accessor)(state).draw_widget(ui, &self.label, rect, zoom, clip_rect)
    }
}

/// Declarative binding between a node-state field and an inline control.
pub struct PropDef<S> {
    pub(crate) id: String,
    /// Row height when rendered in a side panel; `None` uses the
    /// panel's default row height. Controls that need more vertical room
    /// (e.g. a channel grid) set this.
    pub(crate) panel_height: Option<f32>,
    pub(crate) binding: Box<dyn ControlBinding<S>>,
}

impl<S: 'static> PropDef<S> {
    pub fn control<T: InlineControl + 'static>(
        id: impl Into<String>,
        label: impl Into<String>,
        accessor: for<'a> fn(&'a mut S) -> &'a mut T,
    ) -> Self {
        let label = label.into();
        Self {
            id: id.into(),
            panel_height: None,
            binding: Box::new(ControlBindingRenderer { label, accessor }),
        }
    }

    /// Binds a control selected from instance state. Unlike [`Self::control`],
    /// the accessor may capture stable schema data such as an option index.
    pub fn instance_control<T, F>(
        id: impl Into<String>,
        label: impl Into<String>,
        accessor: F,
    ) -> Self
    where
        T: InlineControl + 'static,
        F: for<'a> Fn(&'a mut S) -> &'a mut T + Send + Sync + 'static,
    {
        Self {
            id: id.into(),
            panel_height: None,
            binding: Box::new(InstanceControlBindingRenderer {
                label: label.into(),
                accessor,
                marker: PhantomData,
            }),
        }
    }

    /// Requests a taller row in a side panel.
    pub fn panel_height(mut self, height: f32) -> Self {
        self.panel_height = Some(height);
        self
    }
}

/// A titled, collapsible group of controls in a side panel.
pub struct PanelSection<S> {
    pub title: String,
    pub props: Vec<PropDef<S>>,
}

impl<S> PanelSection<S> {
    pub fn new(title: impl Into<String>, props: Vec<PropDef<S>>) -> Self {
        Self {
            title: title.into(),
            props,
        }
    }
}

/// Complete socket and control schema for one saved node instance.
pub struct NodeInstanceSchema<S> {
    pub inputs: Vec<InputDef<S>>,
    pub outputs: Vec<OutputDef<S>>,
    pub props: Vec<PropDef<S>>,
    pub panel: Vec<PanelSection<S>>,
    pub view_panel: Vec<PanelSection<S>>,
}

impl<S> NodeInstanceSchema<S> {
    pub fn new(inputs: Vec<InputDef<S>>, outputs: Vec<OutputDef<S>>) -> Self {
        Self {
            inputs,
            outputs,
            props: Vec::new(),
            panel: Vec::new(),
            view_panel: Vec::new(),
        }
    }

    pub fn props(mut self, props: Vec<PropDef<S>>) -> Self {
        self.props = props;
        self
    }

    pub fn panel(mut self, panel: Vec<PanelSection<S>>) -> Self {
        self.panel = panel;
        self
    }

    pub fn view_panel(mut self, view_panel: Vec<PanelSection<S>>) -> Self {
        self.view_panel = view_panel;
        self
    }
}

pub trait NodeDef: 'static {
    type State: fmt::Debug + Clone + Serialize + DeserializeOwned + 'static;

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
    fn inputs() -> Vec<InputDef<Self::State>>
    where
        Self: Sized;
    fn outputs() -> Vec<OutputDef<Self::State>>
    where
        Self: Sized;
    fn state() -> Self::State
    where
        Self: Sized;
    /// Returns the deterministic schema for one saved state. Static node
    /// definitions inherit the traditional methods; plugin-owned dynamic
    /// definitions override this method and keep their schema snapshot in
    /// state.
    fn instance_schema(state: &Self::State) -> NodeInstanceSchema<Self::State>
    where
        Self: Sized,
    {
        let _ = state;
        NodeInstanceSchema::new(Self::inputs(), Self::outputs())
            .props(Self::props())
            .panel(Self::panel())
            .view_panel(Self::view_panel())
    }
    fn props() -> Vec<PropDef<Self::State>>
    where
        Self: Sized,
    {
        vec![]
    }
    /// Properties shown in the right-docked properties panel when this node
    /// is active. Edits run through the same state/`on_update` path as
    /// inline controls.
    fn panel() -> Vec<PanelSection<Self::State>>
    where
        Self: Sized,
    {
        vec![]
    }
    /// Viewer-only properties shown in the right-docked View panel when this
    /// node is active. Concrete nodes declare presentation controls here;
    /// the generic graph widget renders them without interpreting their
    /// meaning.
    fn view_panel() -> Vec<PanelSection<Self::State>>
    where
        Self: Sized,
    {
        vec![]
    }
    fn on_update(_state: &mut Self::State, _inputs: &mut [Socket], _outputs: &mut [Socket])
    where
        Self: Sized,
    {
    }
    /// Status message shown under the node, recomputed after every state
    /// update (validation notes, clamped settings, …).
    fn badge(_state: &Self::State) -> Option<NodeBadge>
    where
        Self: Sized,
    {
        None
    }
}
