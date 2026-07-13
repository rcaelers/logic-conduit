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
