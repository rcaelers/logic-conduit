//! Concrete capture source graph nodes.

mod dslogic_u3pro16;
mod file_source;
mod sigrok_file_source;
#[cfg(any(test, feature = "test-support"))]
mod test_capture_source;
#[cfg(any(test, feature = "test-support"))]
mod test_uart_source;

#[cfg(target_arch = "wasm32")]
mod synthetic_presentation;

pub(crate) use dslogic_u3pro16::DsLogicU3Pro16Builder;
pub use dslogic_u3pro16::{CaptureDurationValue, DsLogicU3Pro16, U3Pro16Metadata, U3Pro16State};
pub(crate) use file_source::FileSourceBuilder;
pub use file_source::{DslFileSource, DslFileSourceState};
pub(crate) use sigrok_file_source::SigrokFileSourceBuilder;
pub use sigrok_file_source::{SigrokFileSource, SigrokFileSourceState};
#[cfg(any(test, feature = "test-support"))]
pub use test_capture_source::{TestCaptureSource, TestCaptureSourceState, TestLiveCaptureSource};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use test_capture_source::{TestCaptureSourceBuilder, TestLiveCaptureSourceBuilder};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use test_uart_source::TestUartSourceBuilder;
#[cfg(any(test, feature = "test-support"))]
pub use test_uart_source::{TestUartSource, TestUartSourceState};
