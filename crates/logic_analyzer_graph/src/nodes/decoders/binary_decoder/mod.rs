mod builder;
mod definition;
mod presentation;
mod registration;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use builder::BinaryDecoderBuilder;
#[cfg(test)]
pub(crate) use definition::default_input_strategy;
pub use definition::{BinaryDecoder, BinaryDecoderState};
