//! Capture and synthetic source processing nodes.

#[cfg(not(target_arch = "wasm32"))]
pub mod dsl_file;
#[cfg(not(target_arch = "wasm32"))]
pub mod dslogic_u3pro16;
#[cfg(not(target_arch = "wasm32"))]
pub mod sigrok_file;
pub mod synthetic_capture_source;
pub mod synthetic_uart_source;

#[cfg(not(target_arch = "wasm32"))]
mod capture_archive;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod conformance_tests;
