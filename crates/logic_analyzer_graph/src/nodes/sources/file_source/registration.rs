inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::DslFileSource,
        super::builder::FileSourceBuilder,
    >("org.logicconduit.graph-node.dsl-file-source/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.text-sample/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn dsl_file_source_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.dsl-file-source/v1",
        );
    }
}
