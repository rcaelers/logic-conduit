//! Concrete graph nodes and their registry infrastructure.

mod catalog;
#[cfg(feature = "test-support")]
mod node_test_support;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod platform_registration_tests;
#[cfg(all(test, target_arch = "wasm32"))]
mod platform_registration_web_tests;
mod registration;
mod registry;
#[cfg(test)]
mod test_support;

mod decoders;
mod logic;
mod sinks;
mod sources;

pub(crate) use catalog::standard_builders;
#[cfg(feature = "test-support")]
pub use node_test_support::{apply_registered_live_capture_edit, registered_node_name};
pub use registration::GraphNodeRegistration;
pub(crate) use registration::{graph_node_registrations, validate_graph_node_payload_requirements};
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use registry::test_graphs_tests;
pub use registry::{
    Number, Signal, Text, TextOpenPath, TextSavePath, Trigger, Words, build_registry,
};
#[cfg(test)]
pub(crate) use test_support::{default_node_state, node_builder, node_name};
