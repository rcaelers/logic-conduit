#[test]
fn generic_viewer_sources_contain_no_uart_contracts() {
    let sources = [
        include_str!("channel.rs"),
        include_str!("cursor.rs"),
        include_str!("draw/derived.rs"),
        include_str!("draw/mod.rs"),
        include_str!("lanes.rs"),
        include_str!("viewer.rs"),
    ];
    let forbidden = [
        "uart_data_lane_name",
        "UART",
        "uart_",
        "\"Bits\"",
        "\"Data\"",
        "u64::MAX - 1",
        "u64::MAX - 2",
    ];

    for token in forbidden {
        assert!(
            sources.iter().all(|source| !source.contains(token)),
            "generic viewer source contains protocol-specific token {token:?}"
        );
    }
}

#[test]
fn generic_viewer_exposes_no_decoder_table_contracts() {
    let sources = [include_str!("lib.rs"), include_str!("lanes.rs")];
    let forbidden = ["DecoderTable", "ViewerTable"];

    for token in forbidden {
        assert!(
            sources.iter().all(|source| !source.contains(token)),
            "generic viewer source contains decoder-table contract {token:?}"
        );
    }
}

#[test]
fn generic_viewer_has_no_legacy_collected_lane_fallback() {
    let sources = [
        include_str!("channel.rs"),
        include_str!("cursor.rs"),
        include_str!("draw/derived.rs"),
        include_str!("draw/frame.rs"),
        include_str!("lanes.rs"),
        include_str!("viewer.rs"),
    ];
    let forbidden = ["DerivedLaneData", "LaneSummary", "CollectedValueKind"];

    for token in forbidden {
        assert!(
            sources.iter().all(|source| !source.contains(token)),
            "generic viewer source contains legacy collected-lane fallback {token:?}"
        );
    }
}
