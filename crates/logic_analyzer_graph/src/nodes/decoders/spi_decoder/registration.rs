inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::SpiDecoder,
        super::builder::SpiDecoderBuilder,
    >("org.logicconduit.graph-node.spi-decoder/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.word/v1",
    ])
}
