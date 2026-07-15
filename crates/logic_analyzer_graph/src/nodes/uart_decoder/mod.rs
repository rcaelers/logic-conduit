mod builder;
mod definition;
mod presentation;

pub(crate) use builder::UartDecoderBuilder;
pub use definition::{
    UartDecoder, UartDecoderState, default_baud_preset, default_display_format, selected_baud_rate,
};
pub(crate) use presentation::uart_output_presentation;
