inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::BinaryDecoder,
        super::builder::BinaryDecoderBuilder,
    >("org.logicconduit.graph-node.binary-decoder/v1")
}
