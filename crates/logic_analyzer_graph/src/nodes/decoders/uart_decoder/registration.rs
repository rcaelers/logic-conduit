inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::UartDecoder,
        super::builder::UartDecoderBuilder,
    >("org.logicconduit.graph-node.uart-decoder/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.trigger/v1",
        "org.logicconduit.word/v1",
    ])
}
