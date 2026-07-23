inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::UartDecoder,
        super::builder::UartDecoderBuilder,
    >("org.logicconduit.graph-node.uart-decoder/v1")
}
