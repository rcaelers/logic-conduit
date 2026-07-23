//! Concrete graph nodes and their registry infrastructure.

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

#[cfg(any(test, feature = "test-support"))]
pub(crate) use registry::test_graphs_tests;
#[cfg(test)]
pub(crate) use test_support::node_name;
