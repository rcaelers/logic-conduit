inventory::submit! {
    logic_analyzer_graph_api::node::GraphNodeRegistration::runnable::<
        super::definition::StringFormatter,
        super::builder::FormatterBuilder,
    >("org.logicconduit.graph-node.string-formatter/v1").requiring_payloads(&[
        "org.logicconduit.number-sample/v1",
        "org.logicconduit.text-sample/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn string_formatter_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.string-formatter/v1",
        );
    }
}
