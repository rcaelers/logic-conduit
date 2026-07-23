//! Concrete protocol-decoder graph nodes.

mod binary_decoder;
mod i2c_decoder;
mod spi_decoder;
mod uart_decoder;

#[cfg(test)]
pub(crate) use binary_decoder::BinaryDecoderBuilder;
#[cfg(test)]
pub(crate) use binary_decoder::default_input_strategy;
pub use binary_decoder::{BinaryDecoder, BinaryDecoderState};
pub use i2c_decoder::I2cDecoder;
pub use spi_decoder::{SpiDecoder, SpiDecoderMetadata, SpiDecoderState};
#[cfg(test)]
pub(crate) use uart_decoder::default_baud_preset;
pub(crate) use uart_decoder::default_display_format;
pub use uart_decoder::{UartDecoder, UartDecoderState};
