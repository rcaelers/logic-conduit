inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::FileWriter,
        super::builder::FileWriterBuilder,
    >("org.logicconduit.graph-node.file-writer/v1")
}
