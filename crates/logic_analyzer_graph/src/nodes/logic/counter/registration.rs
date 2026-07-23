inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::Counter,
        super::builder::CounterBuilder,
    >("org.logicconduit.graph-node.counter/v1")
}
