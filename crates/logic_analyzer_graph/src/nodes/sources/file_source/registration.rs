inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::DslFileSource,
        super::builder::FileSourceBuilder,
    >("org.logicconduit.graph-node.dsl-file-source/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.text-sample/v1",
    ])
}
