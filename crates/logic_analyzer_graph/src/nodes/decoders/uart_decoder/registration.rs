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

#[cfg(test)]
mod registration_tests {
    #[test]
    fn uart_decoder_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.uart-decoder/v1",
        );
    }
}
