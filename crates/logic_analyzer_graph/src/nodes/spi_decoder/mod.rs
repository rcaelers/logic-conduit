mod builder;
mod definition;
mod presentation;

pub(crate) use builder::SpiDecoderBuilder;
pub use definition::{SpiDecoder, SpiDecoderMetadata, SpiDecoderState};
pub(crate) use presentation::spi_output_presentation;
