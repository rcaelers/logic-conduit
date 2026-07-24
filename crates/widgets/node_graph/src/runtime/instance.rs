use egui::{Rect, Ui};
use serde_json::Value;

use super::registry::{reconcile_input_sockets, reconcile_output_sockets};
use crate::api::{InputDef, NodeDef, OutputDef, PanelSection, PropDef};
use crate::model::{Node, NodeBadge, Socket};

/// Layout facts about one panel section, including the stable identity and
/// row height of each property.
pub(crate) struct PanelSectionMeta {
    pub(crate) title: String,
    pub(crate) props: Vec<PanelPropMeta>,
}

pub(crate) struct PanelPropMeta {
    pub(crate) id: String,
    pub(crate) height: Option<f32>,
}

pub(crate) trait NodeInstance {
    fn update(&mut self, inputs: &mut Vec<Socket>, outputs: &mut Vec<Socket>);
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
    fn view_panel_sections(&self) -> Vec<PanelSectionMeta>;
    fn draw_panel_prop(
        &mut self,
        section: usize,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
        clip_rect: Rect,
    ) -> bool;
    fn draw_view_panel_prop(
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
    pub view_panel: Vec<PanelSection<T::State>>,
}

impl<T: NodeDef> NodeInstance for TypedNode<T> {
    fn update(&mut self, inputs: &mut Vec<Socket>, outputs: &mut Vec<Socket>) {
        T::on_update(&mut self.state, inputs, outputs);
        let schema = T::instance_schema(&self.state);
        reconcile_input_sockets(inputs, &schema.inputs);
        reconcile_output_sockets(outputs, &schema.outputs);
        self.inputs = schema.inputs;
        self.outputs = schema.outputs;
        self.properties = schema.props;
        self.panel = schema.panel;
        self.view_panel = schema.view_panel;
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
        panel_section_meta(&self.panel)
    }

    fn view_panel_sections(&self) -> Vec<PanelSectionMeta> {
        panel_section_meta(&self.view_panel)
    }

    fn draw_panel_prop(
        &mut self,
        section: usize,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
        clip_rect: Rect,
    ) -> bool {
        draw_panel_prop(
            &mut self.state,
            &self.panel,
            section,
            index,
            ui,
            rect,
            clip_rect,
        )
    }

    fn draw_view_panel_prop(
        &mut self,
        section: usize,
        index: usize,
        ui: &mut Ui,
        rect: Rect,
        clip_rect: Rect,
    ) -> bool {
        draw_panel_prop(
            &mut self.state,
            &self.view_panel,
            section,
            index,
            ui,
            rect,
            clip_rect,
        )
    }

    fn save_state(&self) -> Value {
        serde_json::to_value(&self.state).expect("node state must serialize")
    }
}

fn panel_section_meta<S>(sections: &[PanelSection<S>]) -> Vec<PanelSectionMeta> {
    sections
        .iter()
        .map(|section| PanelSectionMeta {
            title: section.title.clone(),
            props: section
                .props
                .iter()
                .map(|prop| PanelPropMeta {
                    id: prop.id.clone(),
                    height: prop.panel_height,
                })
                .collect(),
        })
        .collect()
}

fn draw_panel_prop<S>(
    state: &mut S,
    sections: &[PanelSection<S>],
    section: usize,
    index: usize,
    ui: &mut Ui,
    rect: Rect,
    clip_rect: Rect,
) -> bool {
    sections
        .get(section)
        .and_then(|section| section.props.get(index))
        .is_some_and(|prop| {
            // Panel widgets render in screen space at full size.
            prop.binding.draw(state, ui, rect, 1.0, clip_rect)
        })
}
