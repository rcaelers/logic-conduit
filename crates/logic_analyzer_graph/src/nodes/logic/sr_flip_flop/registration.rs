inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::SrFlipFlop,
        super::builder::SrFlipFlopBuilder,
    >("org.logicconduit.graph-node.sr-flip-flop/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.trigger/v1",
    ])
}
