inventory::submit! {
    crate::GraphNodeRegistration::definition::<super::definition::I2cDecoder>(
        "org.logicconduit.graph-node.i2c-decoder/v1",
    )
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn i2c_is_discovered_as_an_editor_only_graph_feature() {
        let registration = crate::nodes::graph_node_registrations()
            .into_iter()
            .find(|registration| registration.name() == "I2C Decoder")
            .expect("I2C inventory submission must be linked");

        assert_eq!(
            registration.stable_id(),
            "org.logicconduit.graph-node.i2c-decoder/v1"
        );
        assert!(registration.builder().is_none());
    }
}
