fn implementation_source(source: &'static str) -> &'static str {
    source
        .split_once("#[cfg(test)]")
        .or_else(|| source.split_once("#[cfg(all(test"))
        .map_or(source, |(implementation, _)| implementation)
}

#[test]
fn generic_capture_components_contain_no_provider_or_model_contracts() {
    let sources = [
        ("application", include_str!("../app.rs")),
        ("coordinator contract", include_str!("mod.rs")),
        ("native coordinator", include_str!("native.rs")),
        (
            "compiler",
            include_str!("../../../logic_analyzer_graph/src/compiler/graph.rs"),
        ),
        (
            "capture runtime",
            include_str!("../../../signal_processing/src/live_capture.rs"),
        ),
        (
            "capture store contract",
            include_str!("../../../signal_processing/src/live_capture_store/mod.rs"),
        ),
        (
            "native capture store",
            include_str!("../../../signal_processing/src/live_capture_store/native.rs"),
        ),
        (
            "growing waveform contract",
            include_str!("../../../signal_processing/src/live_capture_waveform/mod.rs"),
        ),
        (
            "viewer",
            include_str!("../../../widgets/logic_analyzer_viewer/src/viewer.rs"),
        ),
    ];
    let forbidden = ["DeterministicFake", "BufferedFake", "U3Pro16", "u3pro16"];

    for (component, source) in sources {
        let source = implementation_source(source);
        for token in forbidden {
            assert!(
                !source.contains(token),
                "generic {component} source contains provider/model-specific token {token:?}"
            );
        }
    }
}
