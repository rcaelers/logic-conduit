inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::BinaryDecoder,
        super::builder::BinaryDecoderBuilder,
    >("org.logicconduit.graph-node.binary-decoder/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.word/v1",
    ])
}
