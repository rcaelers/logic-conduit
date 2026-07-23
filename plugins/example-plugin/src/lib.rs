//! Example compile-time plugin crate proving the graph, runtime, viewer, and
//! application-panel extension contracts with payloads owned outside the host crates.

mod camera_frame;
mod pulse_measure;

pub use camera_frame::{CameraFrame, CameraFrameSocket, CameraFrameSource};
pub use pulse_measure::{PulseMeasure, PulseSocket, PulseWidth};

/// Linker anchor used by a host that enables this compile-time plugin.
pub fn force_link() {}
