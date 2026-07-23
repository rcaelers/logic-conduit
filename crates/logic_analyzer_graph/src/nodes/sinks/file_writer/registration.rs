inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::FileWriter,
        super::builder::FileWriterBuilder,
    >("org.logicconduit.graph-node.file-writer/v1").requiring_payloads(&[
        "org.logicconduit.text-sample/v1",
        "org.logicconduit.word/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn file_writer_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.file-writer/v1",
        );
    }
}
