mod builder;
mod definition;
mod presentation;
mod registration;

#[cfg(test)]
pub(crate) use definition::default_baud_preset;
pub(crate) use definition::default_display_format;
pub use definition::{UartDecoder, UartDecoderState};
