use crate::api::{InputDef, NodeDef, OutputDef, PanelSection, PropDef};
use crate::model::{Node, NodeBadge, Socket};
use egui::{Rect, Ui};
use serde_json::Value;

/// Layout facts about one panel section: title plus the height of each prop
/// row (the panel's default row height unless the def requested more).
pub(crate) struct PanelSectionMeta {
    pub title: &'static str,
    pub prop_heights: Vec<Option<f32>>,
}

pub(crate) trait NodeInstance {
    fn update(&mut self, inputs: &mut [Socket], outputs: &mut [Socket]);
    fn badge(&self) -> Option<NodeBadge>;
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
    fn panel_sections(&self) -> Vec<PanelSectionMeta>;
    fn draw_panel_prop(
        &mut self,
        section: usize,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
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
    pub panel: Vec<PanelSection<T::State>>,
}

impl<T: NodeDef> NodeInstance for TypedNode<T> {
    fn update(&mut self, inputs: &mut [Socket], outputs: &mut [Socket]) {
        T::on_update(&mut self.state, inputs, outputs);
    }

    fn badge(&self) -> Option<NodeBadge> {
        T::badge(&self.state)
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

    fn panel_sections(&self) -> Vec<PanelSectionMeta> {
        self.panel
            .iter()
            .map(|section| PanelSectionMeta {
                title: section.title,
                prop_heights: section.props.iter().map(|prop| prop.panel_height).collect(),
            })
            .collect()
    }

    fn draw_panel_prop(
        &mut self,
        section: usize,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
        clip_rect: Rect,
    ) -> bool {
        self.panel
            .get(section)
            .and_then(|section| section.props.get(index))
            .is_some_and(|prop| {
                // Panel widgets render in screen space at full size.
                prop.binding.draw(&mut self.state, ui, rect, 1.0, clip_rect)
            })
    }

    fn save_state(&self) -> Value {
        serde_json::to_value(&self.state).expect("node state must serialize")
    }
}
