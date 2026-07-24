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

    use logic_analyzer_processing::support::discover_sigrok_decoder;

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
