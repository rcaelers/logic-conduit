inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::FileWriter,
        super::builder::FileWriterBuilder,
    >("org.logicconduit.graph-node.file-writer/v1").requiring_payloads(&[
        "org.logicconduit.text-sample/v1",
        "org.logicconduit.word/v1",
    ])
}
