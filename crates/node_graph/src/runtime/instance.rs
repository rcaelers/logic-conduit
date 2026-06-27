use crate::api::{InputDef, NodeDef, OutputDef, PropDef};
use crate::model::{Node, Socket};
use egui::{Rect, Ui};
use serde_json::Value;

pub(crate) trait NodeInstance {
    fn update(&mut self, inputs: &mut [Socket], outputs: &mut [Socket]);
    fn draw_input_control(
        &mut self,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool;
    fn draw_output_control(
        &mut self,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool;
    fn draw_property(
        &mut self,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool;
    fn save_state(&self) -> Value;
}

pub(crate) struct NodeRuntime {
    pub node: Node,
    pub instance: Box<dyn NodeInstance>,
}

pub(crate) struct TypedNode<T: NodeDef> {
    pub state: T::State,
    pub inputs: Vec<InputDef<T::State>>,
    pub outputs: Vec<OutputDef<T::State>>,
    pub properties: Vec<PropDef<T::State>>,
}

impl<T: NodeDef> NodeInstance for TypedNode<T> {
    fn update(&mut self, inputs: &mut [Socket], outputs: &mut [Socket]) {
        T::on_update(&mut self.state, inputs, outputs);
    }

    fn draw_input_control(
        &mut self,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        self.inputs
            .get(index)
            .and_then(|input| input.control.as_ref())
            .is_some_and(|binding| binding.draw(&mut self.state, ui, rect, zoom, clip_rect))
    }

    fn draw_output_control(
        &mut self,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        self.outputs
            .get(index)
            .and_then(|output| output.control.as_ref())
            .is_some_and(|binding| binding.draw(&mut self.state, ui, rect, zoom, clip_rect))
    }

    fn draw_property(
        &mut self,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        self.properties.get(index).is_some_and(|property| {
            property
                .binding
                .draw(&mut self.state, ui, rect, zoom, clip_rect)
        })
    }

    fn save_state(&self) -> Value {
        serde_json::to_value(&self.state).expect("node state must serialize")
    }
}
