inventory::submit! {
    logic_analyzer_graph_api::node::GraphNodeRegistration::runnable::<
        super::definition::SigrokDecoderDefinition,
        super::builder::SigrokDecoderBuilder,
    >("org.logicconduit.graph-node.sigrok-decoder/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.sigrok.annotation/v1",
        "org.logicconduit.sigrok.binary/v1",
        "org.logicconduit.sigrok.generated-logic/v1",
        "org.logicconduit.sigrok.metadata/v1",
        "org.logicconduit.sigrok.protocol-packet/v1",
    ])
}

#[cfg(test)]
mod registration_tests {
    use std::path::PathBuf;

    use egui::Pos2;

    use logic_analyzer_graph_api::node::RuntimeBuilder;
    use logic_analyzer_processing::support::discover_sigrok_decoder;
    use node_graph::{NodeDef, NodeGraphWidget, NodeTypeRegistry};

    use super::super::builder::SigrokDecoderBuilder;
    use super::super::definition::{SigrokDecoderDefinition, SigrokDecoderState};

    #[test]
    fn standard_spi_decoder_lowers_in_isolation_from_discovered_metadata() {
        let Some(decoder_root) = local_decoder_root() else {
            eprintln!("skipping Sigrok graph-node test: set SIGROK_DECODERS_DIR");
            return;
        };
        let descriptor = discover_sigrok_decoder(&decoder_root, "spi").unwrap();
        let mut state = super::super::definition::SigrokDecoderState::from_descriptor(
            decoder_root,
            &descriptor,
        );
        for channel in &mut state.channels {
            if matches!(channel.id.as_str(), "mosi" | "cs") {
                channel.enabled.value = true;
            }
        }
        crate::nodes::test_support::assert_node_registration_isolated_with_state(
            "org.logicconduit.graph-node.sigrok-decoder/v1",
            Some(serde_json::to_value(state).unwrap()),
        );
    }

    #[test]
    fn standard_stacked_decoder_lowers_with_a_protocol_packet_input() {
        let Some(decoder_root) = local_decoder_root() else {
            eprintln!("skipping Sigrok graph-node test: set SIGROK_DECODERS_DIR");
            return;
        };
        let descriptor = discover_sigrok_decoder(&decoder_root, "spiflash").unwrap();
        assert_eq!(descriptor.inputs, ["spi"]);
        let state = SigrokDecoderState::from_descriptor(decoder_root, &descriptor);
        crate::nodes::test_support::assert_node_registration_isolated_with_state(
            "org.logicconduit.graph-node.sigrok-decoder/v1",
            Some(serde_json::to_value(state).unwrap()),
        );
    }

    #[test]
    fn graph_connection_contracts_follow_declared_protocol_ids() {
        let Some(decoder_root) = local_decoder_root() else {
            eprintln!("skipping Sigrok graph-node test: set SIGROK_DECODERS_DIR");
            return;
        };
        let spi = SigrokDecoderState::from_descriptor(
            decoder_root.clone(),
            &discover_sigrok_decoder(&decoder_root, "spi").unwrap(),
        );
        let spiflash_descriptor = discover_sigrok_decoder(&decoder_root, "spiflash").unwrap();
        let spiflash = SigrokDecoderState::from_descriptor(decoder_root, &spiflash_descriptor);
        let mut registry = NodeTypeRegistry::new();
        registry.register::<SigrokDecoderDefinition>();
        let mut widget = NodeGraphWidget::new(registry);
        let producer = widget
            .add_node_at(SigrokDecoderDefinition::name(), Pos2::ZERO)
            .unwrap();
        let consumer = widget
            .add_node_at(SigrokDecoderDefinition::name(), Pos2::new(100.0, 0.0))
            .unwrap();
        assert!(widget.set_node_state(producer, serde_json::to_value(spi).unwrap()));
        assert!(widget.set_node_state(consumer, serde_json::to_value(spiflash).unwrap()));
        let producer_node = &widget.graph().nodes[&producer];
        let consumer_node = &widget.graph().nodes[&consumer];
        let output = producer_node
            .outputs
            .iter()
            .find(|socket| socket.schema_id == "packets")
            .unwrap();
        let input = consumer_node
            .inputs
            .iter()
            .find(|socket| socket.schema_id == "protocol_packets")
            .unwrap();
        let builder = SigrokDecoderBuilder;
        let offered = builder.offered_connection_contracts(output, &producer_node.state);
        let accepted = builder.accepted_connection_contracts(input, &consumer_node.state);
        assert_eq!(offered, ["spi"]);
        assert_eq!(accepted, ["spi"]);
        assert!(offered.iter().any(|contract| accepted.contains(contract)));
    }

    #[test]
    fn previous_saved_spi_state_gains_protocol_contracts_with_a_warning() {
        let Some(decoder_root) = local_decoder_root() else {
            eprintln!("skipping Sigrok graph-node test: set SIGROK_DECODERS_DIR");
            return;
        };
        let descriptor = discover_sigrok_decoder(&decoder_root, "spi").unwrap();
        let mut state = SigrokDecoderState::from_descriptor(decoder_root.clone(), &descriptor);
        state.schema_version = 1;
        state.protocol_outputs.clear();
        state.catalog.search_paths = decoder_root.display().to_string();

        let mut registry = NodeTypeRegistry::new();
        registry.register::<SigrokDecoderDefinition>();
        let mut widget = NodeGraphWidget::new(registry);
        let node = widget
            .add_node_at(SigrokDecoderDefinition::name(), Pos2::ZERO)
            .unwrap();
        assert!(widget.set_node_state(node, serde_json::to_value(state).unwrap()));
        let migrated: SigrokDecoderState =
            serde_json::from_value(widget.graph().nodes[&node].state.clone()).unwrap();
        assert_eq!(migrated.schema_version, 2);
        assert_eq!(migrated.protocol_outputs, ["spi"]);
        assert!(
            widget.graph().nodes[&node]
                .badge
                .as_ref()
                .is_some_and(|badge| badge.text.contains("protocol connection contracts"))
        );
    }

    fn local_decoder_root() -> Option<PathBuf> {
        std::env::var_os("SIGROK_DECODERS_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                Some(
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../dslogic/libsigrokdecode/decoders"),
                )
            })
            .filter(|path| path.join("spi/pd.py").is_file())
    }
}
