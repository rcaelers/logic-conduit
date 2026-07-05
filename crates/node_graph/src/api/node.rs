use super::control::InlineControl;
use super::socket::{SocketDef, SocketWithControlDef};
use crate::model::{NodeBadge, Socket, SocketShape};
use egui::{Color32, Rect, Ui};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;

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
    pub label: String,
    pub type_name: &'static str,
    /// Idle look shown while unconnected; defaults to the native type's
    /// identity, overridable per def via [`InputDef::idle_style`].
    pub color: Color32,
    pub shape: SocketShape,
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
        Self {
            label: label.into(),
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
    pub label: String,
    pub type_name: &'static str,
    pub color: Color32,
    pub shape: SocketShape,
    pub(crate) identity: SocketTypeIdentity,
    pub(crate) control: Option<Box<dyn ControlBinding<S>>>,
}

impl<S: 'static> OutputDef<S> {
    pub fn new<T: SocketDef>(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
            identity: SocketTypeIdentity::of::<T>(),
            control: None,
        }
    }

    pub fn control<T: SocketWithControlDef>(
        label: impl Into<String>,
        accessor: for<'a> fn(&'a mut S) -> &'a mut T::Control,
    ) -> Self {
        let label = label.into();
        Self {
            label: label.clone(),
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
            identity: SocketTypeIdentity::of::<T>(),
            control: Some(Box::new(ControlBindingRenderer { label, accessor })),
        }
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

/// Declarative binding between a node-state field and an inline control.
pub struct PropDef<S> {
    pub id: &'static str,
    pub label: &'static str,
    /// Row height when rendered in the properties panel; `None` uses the
    /// panel's default row height. Controls that need more vertical room
    /// (e.g. a channel grid) set this.
    pub(crate) panel_height: Option<f32>,
    pub(crate) binding: Box<dyn ControlBinding<S>>,
}

impl<S: 'static> PropDef<S> {
    pub fn control<T: InlineControl + 'static>(
        id: &'static str,
        label: &'static str,
        accessor: for<'a> fn(&'a mut S) -> &'a mut T,
    ) -> Self {
        Self {
            id,
            label,
            panel_height: None,
            binding: Box::new(ControlBindingRenderer {
                label: label.to_owned(),
                accessor,
            }),
        }
    }

    /// Requests a taller row in the properties panel.
    pub fn panel_height(mut self, height: f32) -> Self {
        self.panel_height = Some(height);
        self
    }
}

/// A titled, collapsible group of props in the properties panel (§4.11):
/// the home of low-frequency configuration that would bloat the node body.
pub struct PanelSection<S> {
    pub title: &'static str,
    pub props: Vec<PropDef<S>>,
}

impl<S> PanelSection<S> {
    pub fn new(title: &'static str, props: Vec<PropDef<S>>) -> Self {
        Self { title, props }
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
