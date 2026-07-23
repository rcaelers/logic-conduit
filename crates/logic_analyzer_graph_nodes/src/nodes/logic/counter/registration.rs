inventory::submit! {
    logic_analyzer_graph_api::node::GraphNodeRegistration::runnable::<
        super::definition::Counter,
        super::builder::CounterBuilder,
    >("org.logicconduit.graph-node.counter/v1").requiring_payloads(&[
        "org.logicconduit.number-sample/v1",
        "org.logicconduit.trigger/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn counter_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.counter/v1",
        );
    }
}
