inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::Viewer,
        super::builder::ViewerSubscriptionBuilder,
    >("org.logicconduit.graph-node.viewer/v1")
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn viewer_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.viewer/v1",
        );
    }
}
