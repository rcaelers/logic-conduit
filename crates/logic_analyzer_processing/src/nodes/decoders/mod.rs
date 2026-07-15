//! Protocol decoder processing nodes.
//!
//! Decoders for live data processing using the channel-based architecture.

pub mod parallel_decoder;
pub mod spi_decoder;
pub mod types;
pub mod uart_decoder;

// Re-export common types
// Re-export decoders
pub use parallel_decoder::{
    ParallelDecoder, ParallelDecoderMetrics, ParallelDecoderMetricsSnapshot,
};
pub use spi_decoder::SpiDecoder;
pub use types::{BitOrder, CsPolarity, Endianness, ParallelInputStrategy, SpiMode, StrobeMode};
pub use uart_decoder::{UartDecoder, UartParity, UartStopBits};
