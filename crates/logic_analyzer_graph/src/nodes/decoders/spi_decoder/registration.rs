inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::SpiDecoder,
        super::builder::SpiDecoderBuilder,
    >("org.logicconduit.graph-node.spi-decoder/v1")
}
