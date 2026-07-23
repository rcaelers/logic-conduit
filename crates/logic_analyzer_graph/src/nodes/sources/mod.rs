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
