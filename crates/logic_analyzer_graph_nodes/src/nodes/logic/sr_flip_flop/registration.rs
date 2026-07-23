inventory::submit! {
    logic_analyzer_graph_api::node::GraphNodeRegistration::runnable::<
        super::definition::SrFlipFlop,
        super::builder::SrFlipFlopBuilder,
    >("org.logicconduit.graph-node.sr-flip-flop/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.trigger/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn sr_flip_flop_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.sr-flip-flop/v1",
        );
    }
}
