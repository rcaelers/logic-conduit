inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::Buffer,
        super::builder::BufferBuilder,
    >("org.logicconduit.graph-node.buffer/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.number-sample/v1",
        "org.logicconduit.text-sample/v1",
        "org.logicconduit.trigger/v1",
        "org.logicconduit.word/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn buffer_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.buffer/v1",
        );
    }
}
