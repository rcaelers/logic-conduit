//! Protocol decoder processing nodes.
//!
//! Decoders for live data processing using the channel-based architecture.

mod parallel_decoder;
mod spi_decoder;
mod uart_decoder;

pub use parallel_decoder::{
    ParallelDecoder, ParallelDecoderMetrics, ParallelDecoderMetricsSnapshot, ParallelInputStrategy,
    StrobeMode,
};
pub use spi_decoder::{SpiDecoder, SpiMode};
pub use uart_decoder::{UartDecoder, UartParity, UartStopBits};
