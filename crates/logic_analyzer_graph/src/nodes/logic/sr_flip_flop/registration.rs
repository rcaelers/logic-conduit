inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::SrFlipFlop,
        super::builder::SrFlipFlopBuilder,
    >("org.logicconduit.graph-node.sr-flip-flop/v1")
}
