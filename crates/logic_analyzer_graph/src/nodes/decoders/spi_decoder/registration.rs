inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::SpiDecoder,
        super::builder::SpiDecoderBuilder,
    >("org.logicconduit.graph-node.spi-decoder/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.word/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn spi_decoder_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.spi-decoder/v1",
        );
    }
}
