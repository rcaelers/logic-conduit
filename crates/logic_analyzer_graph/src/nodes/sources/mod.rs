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

pub use dslogic_u3pro16::{CaptureDurationValue, DsLogicU3Pro16, U3Pro16Metadata, U3Pro16State};
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use file_source::FileSourceBuilder;
pub use file_source::{DslFileSource, DslFileSourceState};
pub use sigrok_file_source::{SigrokFileSource, SigrokFileSourceState};
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use test_capture_source::TestCaptureSourceBuilder;
#[cfg(any(test, feature = "test-support"))]
pub use test_capture_source::{TestCaptureSource, TestCaptureSourceState, TestLiveCaptureSource};
#[cfg(any(test, feature = "test-support"))]
pub use test_uart_source::{TestUartSource, TestUartSourceState};
