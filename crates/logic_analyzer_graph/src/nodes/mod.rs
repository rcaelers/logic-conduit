//! Concrete graph nodes and their registry infrastructure.

#[cfg(feature = "test-support")]
mod node_test_support;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod platform_registration_tests;
#[cfg(all(test, target_arch = "wasm32"))]
mod platform_registration_web_tests;
mod registry;
#[cfg(test)]
mod test_support;

mod decoders;
mod logic;
mod sinks;
mod sources;

#[cfg(feature = "test-support")]
pub use node_test_support::{apply_registered_live_capture_edit, registered_node_name};
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use registry::test_graphs_tests;
pub use registry::{Number, Signal, Text, TextOpenPath, TextSavePath, Trigger, Words};
#[cfg(test)]
pub(crate) use test_support::{default_node_state, node_builder, node_name};

pub use crate::compiler::{GraphNodeRegistration, build_node_registry as build_registry};
