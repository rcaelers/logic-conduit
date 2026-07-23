inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::DsLogicU3Pro16,
        super::builder::DsLogicU3Pro16Builder,
    >("org.logicconduit.graph-node.dslogic-u3pro16/v1")
    .requiring_payloads(&["org.logicconduit.digital-sample/v1"])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn dslogic_u3pro16_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.dslogic-u3pro16/v1",
        );
    }
}
