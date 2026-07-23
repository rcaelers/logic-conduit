inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::CsvWriter,
        super::builder::CsvWriterBuilder,
    >("org.logicconduit.graph-node.csv-writer/v1").requiring_payloads(&[
        "org.logicconduit.text-sample/v1",
        "org.logicconduit.word/v1",
    ])
}
