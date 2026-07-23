inventory::submit! {
    logic_analyzer_graph_api::node::GraphNodeRegistration::runnable::<
        super::definition::SigrokFileSource,
        super::builder::SigrokFileSourceBuilder,
    >("org.logicconduit.graph-node.sigrok-file-source/v1")
    .requiring_payloads(&["org.logicconduit.digital-sample/v1"])
}

#[cfg(test)]
mod registration_tests {
    use node_graph::NodeDef;

    #[test]
    fn sigrok_file_source_lowers_in_isolation() {
        let mut state = super::super::definition::SigrokFileSource::state();
        state.demo_data = true;
        crate::nodes::test_support::assert_node_registration_isolated_with_state(
            "org.logicconduit.graph-node.sigrok-file-source/v1",
            Some(serde_json::to_value(state).expect("test state is serializable")),
        );
    }
}
