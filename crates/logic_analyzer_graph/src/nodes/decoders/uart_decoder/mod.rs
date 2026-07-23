mod builder;
mod definition;
mod presentation;
mod registration;

#[cfg(test)]
pub(crate) use definition::default_baud_preset;
#[cfg(test)]
pub(crate) use definition::{UartDecoder, UartDecoderState};
