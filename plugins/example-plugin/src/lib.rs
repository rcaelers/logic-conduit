//! Example compile-time plugin crate proving the graph, runtime, viewer, and
//! application-panel extension contracts with payloads owned outside the host crates.

mod camera_frame;
mod pulse_measure;

pub use camera_frame::{CameraFrame, CameraFrameSocket, CameraFrameSource};
pub use pulse_measure::{PulseMeasure, PulseMeasureBuilder, PulseSocket, PulseWidth};

/// Registers this plugin's graph/runtime extensions and application panel.
pub fn register(ctx: &mut logic_analyzer_ui::PluginContext) {
    register_graph(ctx.graph());
    camera_frame::register_panel(ctx).expect("example camera panel registration must be unique");
}

fn register_graph(ctx: &mut logic_analyzer_graph::PluginContext) {
    ctx.register_payload::<PulseWidth>()
        .register_node::<PulseMeasure>()
        .register_builder("Pulse Measure", Box::new(PulseMeasureBuilder));
    camera_frame::register_graph(ctx).expect("example camera payload registration must be unique");
}
