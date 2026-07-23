inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::TextFileWriter,
        super::builder::TextFileWriterBuilder,
    >("org.logicconduit.graph-node.text-file-writer/v1")
    .requiring_payloads(&["org.logicconduit.text-sample/v1"])
}
