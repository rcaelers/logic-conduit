inventory::submit! {
    logic_analyzer_graph_api::node::GraphNodeRegistration::runnable::<
        super::definition::TestCaptureSource,
        super::builder::TestCaptureSourceBuilder,
    >("org.logicconduit.graph-node.test-capture-source/v1")
    .requiring_payloads(&["org.logicconduit.digital-sample/v1"])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn capture_sources_lower_in_isolation() {
        for stable_id in [
            "org.logicconduit.graph-node.test-capture-source/v1",
            "org.logicconduit.graph-node.test-live-capture-source/v1",
        ] {
            crate::nodes::test_support::assert_node_registration_isolated(stable_id);
        }
    }
}

inventory::submit! {
    logic_analyzer_graph_api::node::GraphNodeRegistration::runnable::<
        super::definition::TestLiveCaptureSource,
        super::live_builder::TestLiveCaptureSourceBuilder,
    >("org.logicconduit.graph-node.test-live-capture-source/v1")
    .requiring_payloads(&["org.logicconduit.digital-sample/v1"])
}
