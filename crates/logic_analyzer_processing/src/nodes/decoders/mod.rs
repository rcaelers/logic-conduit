//! Protocol decoder processing nodes.
//!
//! Decoders for live data processing using the channel-based architecture.

mod parallel_decoder;
mod spi_decoder;
mod types;
mod uart_decoder;

// Re-export common types
// Re-export decoders
pub use parallel_decoder::{
    ParallelDecoder, ParallelDecoderMetrics, ParallelDecoderMetricsSnapshot,
};
pub use spi_decoder::SpiDecoder;
pub use types::{BitOrder, CsPolarity, Endianness, ParallelInputStrategy, SpiMode, StrobeMode};
pub use uart_decoder::{UartDecoder, UartParity, UartStopBits};
