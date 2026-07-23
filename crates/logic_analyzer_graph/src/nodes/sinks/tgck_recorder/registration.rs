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
