//! Contracts implemented or submitted by graph nodes and compile-time plugins.

mod contracts;
mod graph_registration;
mod payload_registration;

pub use contracts::{CaptureGraphSourceFactory, LiveCaptureFeature, RuntimeBuilder};
pub use graph_registration::GraphNodeRegistration;
pub use payload_registration::CollectedPayloadRegistration;
