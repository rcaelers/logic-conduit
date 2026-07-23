inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::TestUartSource,
        super::builder::TestUartSourceBuilder,
    >("org.logicconduit.graph-node.test-uart-source/v1")
}
