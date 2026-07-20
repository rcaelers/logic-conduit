//! Example compile-time plugin crate — proves the plugin extension seams
//! opened up for it: a new runtime payload type, a new compiler `PortValue`,
//! a new graph `SocketDef`, a `NodeDef` reusing a host-crate socket type,
//! and a matching `RuntimeBuilder`. See [`register`].

mod pulse_measure;

pub use pulse_measure::{PulseMeasure, PulseMeasureBuilder, PulseSocket, PulseWidth};

/// Registers this plugin's payload type, node type, and builder — one
/// consistent, chainable API instead of juggling three different registries
/// by hand (see [`logic_analyzer_graph::PluginContext`]).
pub fn register(ctx: &mut logic_analyzer_graph::PluginContext) {
    ctx.register_payload::<PulseWidth>()
        .register_node::<PulseMeasure>()
        .register_builder("Pulse Measure", Box::new(PulseMeasureBuilder));
}
