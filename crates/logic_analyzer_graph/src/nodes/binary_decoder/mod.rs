mod builder;
mod definition;

pub(crate) use builder::BinaryDecoderBuilder;
pub use definition::{BinaryDecoder, BinaryDecoderState, default_input_strategy};
