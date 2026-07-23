inventory::submit! {
    logic_analyzer_graph_api::node::GraphNodeRegistration::runnable::<
        super::definition::CsvWriter,
        super::builder::CsvWriterBuilder,
    >("org.logicconduit.graph-node.csv-writer/v1").requiring_payloads(&[
        "org.logicconduit.text-sample/v1",
        "org.logicconduit.word/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn csv_writer_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.csv-writer/v1",
        );
    }
}
