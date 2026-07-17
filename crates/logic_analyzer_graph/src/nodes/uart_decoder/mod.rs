mod builder;
mod definition;
mod presentation;

pub(crate) use builder::UartDecoderBuilder;
#[cfg(test)]
pub(crate) use definition::default_baud_preset;
pub use definition::{UartDecoder, UartDecoderState, default_display_format, selected_baud_rate};
pub(crate) use presentation::uart_output_presentation;
