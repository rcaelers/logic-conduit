//! Protocol-neutral decoder-table presentation contracts.

mod contract;
mod subscription;

pub use contract::{DecoderTableColumn, DecoderTableRegistry, DecoderTableSource};
pub(crate) use subscription::subscribe_collected_tables;
