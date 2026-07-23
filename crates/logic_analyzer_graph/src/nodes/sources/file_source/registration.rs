inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::DslFileSource,
        super::builder::FileSourceBuilder,
    >("org.logicconduit.graph-node.dsl-file-source/v1")
}
