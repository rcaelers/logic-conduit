inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::LogicGate,
        super::builder::LogicGateBuilder,
    >("org.logicconduit.graph-node.logic-gate/v1")
}
