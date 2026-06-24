use crate::definition::{InputDef, NodeDef, OutputDef, PropDef};
use crate::graph::{Node, NodeId, NodeKind, Socket};
use egui::{Pos2, Rect, Ui};
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

struct TypedNode<T: NodeDef> {
    state: T::State,
    inputs: Vec<InputDef<T::State>>,
    outputs: Vec<OutputDef<T::State>>,
    properties: Vec<PropDef<T::State>>,
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

fn build_node<T: NodeDef>(id: NodeId, pos: Pos2, state: T::State) -> Node {
    let inputs = T::inputs();
    let outputs = T::outputs();
    let properties = T::props();
    let state_json = serde_json::to_value(&state).expect("node state must serialize");
    let input_sockets = inputs
        .iter()
        .map(|input| Socket {
            name: input.label.clone(),
            type_name: input.type_name.to_owned(),
            color: input.color,
            shape: input.shape,
            visible: true,
            hidden: false,
            has_control: input.control.is_some(),
        })
        .collect();
    let output_sockets = outputs
        .iter()
        .map(|output| Socket {
            name: output.label.clone(),
            type_name: output.type_name.to_owned(),
            color: output.color,
            shape: output.shape,
            visible: true,
            hidden: false,
            has_control: output.control.is_some(),
        })
        .collect();
    let mut node = Node {
        id,
        kind: NodeKind::Regular,
        title: T::name().to_owned(),
        header_color: T::color(),
        pos,
        inputs: input_sockets,
        outputs: output_sockets,
        state: state_json,
        property_count: properties.len(),
        selected: false,
        instance: Some(Box::new(TypedNode::<T> {
            state,
            inputs,
            outputs,
            properties,
        })),
    };
    node.run_update();
    node.sync_state();
    node
}

fn create_node<T: NodeDef>(id: NodeId, pos: Pos2) -> Node {
    build_node::<T>(id, pos, T::state())
}

fn restore_node<T: NodeDef>(node: &mut Node) {
    let state = serde_json::from_value(node.state.clone()).unwrap_or_else(|_| T::state());
    let inputs = T::inputs();
    let outputs = T::outputs();
    let properties = T::props();

    if node.inputs.len() != inputs.len() {
        node.inputs = inputs
            .iter()
            .map(|input| Socket {
                name: input.label.clone(),
                type_name: input.type_name.to_owned(),
                color: input.color,
                shape: input.shape,
                visible: true,
                hidden: false,
                has_control: input.control.is_some(),
            })
            .collect();
    } else {
        for (socket, definition) in node.inputs.iter_mut().zip(&inputs) {
            socket.has_control = definition.control.is_some();
        }
    }
    if node.outputs.len() != outputs.len() {
        node.outputs = outputs
            .iter()
            .map(|output| Socket {
                name: output.label.clone(),
                type_name: output.type_name.to_owned(),
                color: output.color,
                shape: output.shape,
                visible: true,
                hidden: false,
                has_control: output.control.is_some(),
            })
            .collect();
    } else {
        for (socket, definition) in node.outputs.iter_mut().zip(&outputs) {
            socket.has_control = definition.control.is_some();
        }
    }

    node.property_count = properties.len();
    node.instance = Some(Box::new(TypedNode::<T> {
        state,
        inputs,
        outputs,
        properties,
    }));
    node.run_update();
    node.sync_state();
}

pub(crate) struct RegisteredNodeType {
    pub name: String,
    pub category: String,
    pub create: fn(NodeId, Pos2) -> Node,
    pub restore: fn(&mut Node),
}

impl RegisteredNodeType {
    pub fn from_def<T: NodeDef>() -> Self {
        Self {
            name: T::name().to_owned(),
            category: T::category().to_owned(),
            create: create_node::<T>,
            restore: restore_node::<T>,
        }
    }
}
