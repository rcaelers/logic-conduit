inventory::submit! {
    crate::GraphNodeRegistration::definition::<super::definition::I2cDecoder>(
        "org.logicconduit.graph-node.i2c-decoder/v1",
    )
}

#[cfg(test)]
mod registration_tests {
    #[test]
    fn i2c_definition_is_isolated() {
        crate::nodes::test_support::assert_node_registration_isolated(
            "org.logicconduit.graph-node.i2c-decoder/v1",
        );
    }
}
