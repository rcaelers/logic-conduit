//! Concrete capture source graph nodes.

mod demo_capture_source;
mod file_source;
mod uart_demo_source;

#[cfg(not(target_arch = "wasm32"))]
mod dslogic_u3pro16;
#[cfg(not(target_arch = "wasm32"))]
mod sigrok_file_source;

pub(crate) use demo_capture_source::DemoCaptureSourceBuilder;
pub use demo_capture_source::{DemoCaptureSource, DemoCaptureSourceState};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use dslogic_u3pro16::DsLogicU3Pro16Builder;
#[cfg(not(target_arch = "wasm32"))]
pub use dslogic_u3pro16::{DsLogicU3Pro16, U3Pro16Metadata, U3Pro16State};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use file_source::FileSourceBuilder;
pub use file_source::{DslFileSource, DslFileSourceState};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use sigrok_file_source::SigrokFileSourceBuilder;
#[cfg(not(target_arch = "wasm32"))]
pub use sigrok_file_source::{SigrokFileSource, SigrokFileSourceState};
pub(crate) use uart_demo_source::UartDemoSourceBuilder;
pub use uart_demo_source::{UartDemoSource, UartDemoSourceState};
