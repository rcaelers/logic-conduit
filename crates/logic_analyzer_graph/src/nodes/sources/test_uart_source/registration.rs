inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::TestUartSource,
        super::builder::TestUartSourceBuilder,
    >("org.logicconduit.graph-node.test-uart-source/v1")
    .requiring_payloads(&["org.logicconduit.digital-sample/v1"])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn test_uart_source_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.test-uart-source/v1",
        );
    }
}
