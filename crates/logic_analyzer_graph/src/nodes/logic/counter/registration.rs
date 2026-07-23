inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::Counter,
        super::builder::CounterBuilder,
    >("org.logicconduit.graph-node.counter/v1").requiring_payloads(&[
        "org.logicconduit.number-sample/v1",
        "org.logicconduit.trigger/v1",
    ])
}
