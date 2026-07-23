fn implementation_source(source: &'static str) -> &'static str {
    source
        .split_once("#[cfg(test)]")
        .or_else(|| source.split_once("#[cfg(all(test"))
        .map_or(source, |(implementation, _)| implementation)
}

#[test]
fn generic_collection_compiler_has_no_builtin_payload_or_protocol_checks() {
    let sources = [
        ("graph lowering", include_str!("graph.rs")),
        ("data collector", include_str!("data_collector.rs")),
        ("saved subscriptions", include_str!("saved_graph.rs")),
    ];
    let forbidden = [
        "CollectedDataKind",
        "CollectedValueKind",
        "DerivedLaneData",
        "org.logicconduit.digital-sample",
        "org.logicconduit.word",
        "org.logicconduit.trigger",
        "org.logicconduit.number-sample",
        "org.logicconduit.text-sample",
        "\"SPI Decoder\"",
        "\"Binary Decoder\"",
        "\"UART Decoder\"",
        "\"Bits\"",
        "\"Data\"",
    ];

    for (component, source) in sources {
        let source = implementation_source(source);
        for token in forbidden {
            assert!(
                !source.contains(token),
                "generic compiler {component} contains built-in payload or protocol token {token:?}"
            );
        }
    }
}
