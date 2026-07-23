inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::TgckRecorder,
        super::builder::TgckRecorderBuilder,
    >("org.logicconduit.graph-node.tgck-recorder/v1")
}
