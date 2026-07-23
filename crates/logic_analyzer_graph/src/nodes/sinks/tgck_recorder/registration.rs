inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::TgckRecorder,
        super::builder::TgckRecorderBuilder,
    >("org.logicconduit.graph-node.tgck-recorder/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.text-sample/v1",
        "org.logicconduit.word/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn tgck_recorder_lowers_in_isolation() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.tgck-recorder/v1",
        );
    }
}
