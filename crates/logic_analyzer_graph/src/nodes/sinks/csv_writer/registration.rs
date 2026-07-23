inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::CsvWriter,
        super::builder::CsvWriterBuilder,
    >("org.logicconduit.graph-node.csv-writer/v1")
}
