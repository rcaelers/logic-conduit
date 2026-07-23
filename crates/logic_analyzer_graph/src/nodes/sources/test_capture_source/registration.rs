inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::TestCaptureSource,
        super::builder::TestCaptureSourceBuilder,
    >("org.logicconduit.graph-node.test-capture-source/v1")
}
