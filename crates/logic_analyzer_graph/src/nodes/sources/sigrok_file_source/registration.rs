inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::SigrokFileSource,
        super::builder::SigrokFileSourceBuilder,
    >("org.logicconduit.graph-node.sigrok-file-source/v1")
    .requiring_payloads(&["org.logicconduit.digital-sample/v1"])
}
