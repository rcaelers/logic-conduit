//! Built-in LogicConduit graph nodes and collected-payload presentations.

mod collected_payloads;
mod link;
mod nodes;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use link::link;
