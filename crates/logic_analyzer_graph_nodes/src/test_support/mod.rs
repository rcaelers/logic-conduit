//! Cross-crate fixtures for compiler and application integration tests.

mod implementation;

pub use implementation::{
    apply_registered_live_capture_edit, build_binary_decoder_demo, build_live_binary_test,
    build_registry, default_node_state, node_builder, node_name, populate_startup,
    populate_uart_demo, registered_node_name,
};
