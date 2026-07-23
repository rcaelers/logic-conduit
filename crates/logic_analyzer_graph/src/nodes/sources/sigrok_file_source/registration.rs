inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::SigrokFileSource,
        super::builder::SigrokFileSourceBuilder,
    >("org.logicconduit.graph-node.sigrok-file-source/v1")
}
