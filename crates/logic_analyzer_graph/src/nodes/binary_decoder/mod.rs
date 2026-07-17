mod builder;
mod definition;

pub(crate) use builder::BinaryDecoderBuilder;
#[cfg(test)]
pub(crate) use definition::default_input_strategy;
pub use definition::{BinaryDecoder, BinaryDecoderState};
