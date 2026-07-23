//! Logic-analyzer graph-to-runtime compiler and application-host services.
//!
//! This crate lowers a generic [`node_graph`] document through inventory-submitted node contracts
//! into the UI-independent [`signal_processing`] runtime. Concrete graph nodes and their
//! presentations live in `logic-analyzer-graph-nodes`; application composition and window
//! integration belong in `logic-analyzer-ui`.

mod compiler;
mod decoder_table;
pub mod host;
#[cfg(test)]
mod nodes;
