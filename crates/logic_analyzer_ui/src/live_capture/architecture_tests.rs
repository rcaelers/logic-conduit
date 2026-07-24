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
        ("coordinator contract", include_str!("implementation.rs")),
        ("native coordinator", include_str!("native.rs")),
        (
            "compiler",
            include_str!("../../../logic_analyzer_graph/src/compiler/graph.rs"),
        ),
        (
            "capture runtime",
            include_str!("../../../signal_processing/src/live_capture/implementation.rs"),
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
            include_str!("../../../signal_processing/src/waveform_index/mod.rs"),
        ),
        (
            "viewer",
            include_str!("../../../widgets/logic_analyzer_viewer/src/viewer.rs"),
        ),
        (
            "trigger editor",
            include_str!("../../../widgets/trigger_editor/src/lib.rs"),
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

#[test]
fn generic_trigger_editor_contains_no_provider_or_protocol_cases() {
    let source = implementation_source(include_str!("../../../widgets/trigger_editor/src/lib.rs"));
    for token in [
        "U3Pro16",
        "DSLogic",
        "SPI",
        "UART",
        "Binary Decoder",
        "demo:",
    ] {
        assert!(
            !source.contains(token),
            "generic trigger editor contains concrete token {token:?}"
        );
    }
}

#[test]
fn generic_ui_compiler_and_widgets_contain_no_sigrok_host_cases() {
    let sources = [
        ("application", include_str!("../app.rs")),
        (
            "compiler",
            include_str!("../../../logic_analyzer_graph/src/compiler/graph.rs"),
        ),
        (
            "node graph definition API",
            include_str!("../../../widgets/node_graph/src/api/node.rs"),
        ),
        (
            "node graph registry",
            include_str!("../../../widgets/node_graph/src/runtime/registry.rs"),
        ),
        (
            "viewer",
            include_str!("../../../widgets/logic_analyzer_viewer/src/viewer.rs"),
        ),
    ];
    for (component, source) in sources {
        let source = implementation_source(source);
        for token in ["Sigrok", "sigrok", "Python decoder"] {
            assert!(
                !source.contains(token),
                "generic {component} contains concrete decoder-host token {token:?}"
            );
        }
    }
}
