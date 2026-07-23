inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::Viewer,
        super::builder::ViewerSubscriptionBuilder,
    >("org.logicconduit.graph-node.viewer/v1")
}
