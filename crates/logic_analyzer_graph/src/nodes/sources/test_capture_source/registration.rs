inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::TestCaptureSource,
        super::builder::TestCaptureSourceBuilder,
    >("org.logicconduit.graph-node.test-capture-source/v1")
}

inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::TestLiveCaptureSource,
        super::live_builder::TestLiveCaptureSourceBuilder,
    >("org.logicconduit.graph-node.test-live-capture-source/v1")
}
