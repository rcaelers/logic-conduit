//! Logic-analyzer node catalog and graph-to-runtime compiler.
//!
//! This crate is the product-specific bridge between a generic [`node_graph`]
//! document and the UI-independent [`signal_processing`] runtime. Concrete
//! node viewer-lane adapters also live here; application composition and
//! window integration belong in `logic-analyzer-ui`.

pub mod compiler;
pub mod nodes;
mod viewer_lanes;
