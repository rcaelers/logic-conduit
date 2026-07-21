//! Test-only UART signal-source graph node.

mod builder;
mod definition;

pub(crate) use builder::TestUartSourceBuilder;
pub use definition::{TestUartSource, TestUartSourceState};
