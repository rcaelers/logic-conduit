//! Test-only UART signal-source graph node.

mod builder;
mod definition;
mod registration;

#[cfg(test)]
pub(crate) use definition::{TestUartSource, TestUartSourceState};
