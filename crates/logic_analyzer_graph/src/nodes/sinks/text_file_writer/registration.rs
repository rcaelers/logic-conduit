inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::TextFileWriter,
        super::builder::TextFileWriterBuilder,
    >("org.logicconduit.graph-node.text-file-writer/v1")
    .requiring_payloads(&["org.logicconduit.text-sample/v1"])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn text_file_writer_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.text-file-writer/v1",
        );
    }
}
