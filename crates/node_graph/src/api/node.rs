use super::control::InlineControl;
use super::socket::{SocketDef, SocketWithControlDef};
use crate::model::{Socket, SocketShape};
use egui::{Color32, Rect, Ui};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;

pub struct InputDef<S> {
    pub label: String,
    pub type_name: &'static str,
    pub color: Color32,
    pub shape: SocketShape,
    pub(crate) control: Option<Box<dyn ControlBinding<S>>>,
}

impl<S: 'static> InputDef<S> {
    pub fn new<T: SocketDef>(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
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
            control: Some(Box::new(ControlBindingRenderer { label, accessor })),
        }
    }
}

pub struct OutputDef<S> {
    pub label: String,
    pub type_name: &'static str,
    pub color: Color32,
    pub shape: SocketShape,
    pub(crate) control: Option<Box<dyn ControlBinding<S>>>,
}

impl<S: 'static> OutputDef<S> {
    pub fn new<T: SocketDef>(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            type_name: T::type_name(),
            color: T::color(),
            shape: T::shape(),
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
            binding: Box::new(ControlBindingRenderer {
                label: label.to_owned(),
                accessor,
            }),
        }
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
    fn on_update(_state: &mut Self::State, _inputs: &mut [Socket], _outputs: &mut [Socket])
    where
        Self: Sized,
    {
    }
}
