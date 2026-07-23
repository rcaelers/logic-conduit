inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::DsLogicU3Pro16,
        super::builder::DsLogicU3Pro16Builder,
    >("org.logicconduit.graph-node.dslogic-u3pro16/v1")
    .requiring_payloads(&["org.logicconduit.digital-sample/v1"])
}
