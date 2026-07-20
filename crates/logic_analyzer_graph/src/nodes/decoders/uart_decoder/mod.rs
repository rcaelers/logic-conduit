mod builder;
mod definition;
mod presentation;

pub(crate) use builder::UartDecoderBuilder;
#[cfg(test)]
pub(crate) use definition::default_baud_preset;
pub(crate) use definition::default_display_format;
pub use definition::{UartDecoder, UartDecoderState};
