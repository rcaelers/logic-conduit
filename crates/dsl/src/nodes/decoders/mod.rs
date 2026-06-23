//! Protocol decoder nodes
//!
//! Decoders for live data processing using the channel-based architecture.

pub mod parallel_decoder;
pub mod spi_decoder;
pub mod types;

// Re-export common types
pub use types::{CsPolarity, ParallelWord, SpiMode, SpiTransfer, StrobeMode, TimingInfo};

// Re-export decoders
pub use parallel_decoder::ParallelDecoder;
pub use spi_decoder::SpiDecoder;
