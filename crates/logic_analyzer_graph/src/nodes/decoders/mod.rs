//! Concrete protocol-decoder graph nodes.

mod binary_decoder;
mod display_format;
mod i2c_decoder;
mod spi_decoder;
mod uart_decoder;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use binary_decoder::BinaryDecoderBuilder;
#[cfg(test)]
pub(crate) use binary_decoder::default_input_strategy;
#[cfg(test)]
pub(crate) use binary_decoder::{BinaryDecoder, BinaryDecoderState};
#[cfg(test)]
pub(crate) use display_format::default_display_format;
#[cfg(test)]
pub(crate) use spi_decoder::{SpiDecoder, SpiDecoderMetadata, SpiDecoderState};
#[cfg(test)]
pub(crate) use uart_decoder::default_baud_preset;
#[cfg(test)]
pub(crate) use uart_decoder::{UartDecoder, UartDecoderState};
