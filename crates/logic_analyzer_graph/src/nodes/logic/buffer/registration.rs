inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::Buffer,
        super::builder::BufferBuilder,
    >("org.logicconduit.graph-node.buffer/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.number-sample/v1",
        "org.logicconduit.text-sample/v1",
        "org.logicconduit.trigger/v1",
        "org.logicconduit.word/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn buffer_is_discovered_as_one_atomic_graph_feature() {
        let registration = crate::nodes::graph_node_registrations()
            .into_iter()
            .find(|registration| registration.name() == "Buffer")
            .expect("Buffer inventory submission must be linked");

        assert_eq!(
            registration.stable_id(),
            "org.logicconduit.graph-node.buffer/v1"
        );
        let _builder = registration
            .builder()
            .expect("Buffer inventory submission must include its builder");
    }
}
