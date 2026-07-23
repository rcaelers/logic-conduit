mod builder;
mod definition;
mod presentation;
mod registration;

#[cfg(test)]
pub(crate) use definition::SpiDecoder;
#[cfg(test)]
pub(crate) use definition::{SpiDecoderMetadata, SpiDecoderState};
