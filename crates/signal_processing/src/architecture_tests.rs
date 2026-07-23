fn implementation_source(source: &'static str) -> &'static str {
    source
        .split_once("#[cfg(test)]")
        .or_else(|| source.split_once("#[cfg(all(test"))
        .map_or(source, |(implementation, _)| implementation)
}

#[test]
fn generic_runtime_contains_no_concrete_source_or_protocol_contracts() {
    let sources = [
        ("samples", include_str!("sample.rs")),
        ("scheduler", include_str!("scheduler.rs")),
        ("edge queries", include_str!("edge_query.rs")),
        ("senders", include_str!("sender.rs")),
        ("ports", include_str!("ports.rs")),
        (
            "cooperative manager",
            include_str!("cooperative_manager.rs"),
        ),
        ("threaded manager", include_str!("manager.rs")),
        ("events", include_str!("events.rs")),
        (
            "derived-data collector",
            include_str!("derived_data_collector.rs"),
        ),
    ];
    let forbidden = [
        "DslFileSource",
        "LogicAnalyzerSource",
        "DSLogic",
        "U3Pro16",
        "Binary Decoder",
        "SPI",
        "UART",
        "I2C",
    ];

    for (component, source) in sources {
        let source = implementation_source(source);
        for token in forbidden {
            assert!(
                !source.contains(token),
                "generic {component} source contains concrete token {token:?}"
            );
        }
    }
}

#[test]
fn type_erased_collection_contract_has_no_builtin_payload_checks() {
    let source = implementation_source(include_str!("collected_payload.rs"));
    for token in [
        "CollectedDataKind",
        "CollectedValueKind",
        "DerivedLaneData",
        "org.logicconduit.",
        "SPI",
        "UART",
        "Binary Decoder",
    ] {
        assert!(
            !source.contains(token),
            "generic collected-payload contract contains built-in token {token:?}"
        );
    }
}

#[test]
fn generic_storage_does_not_choose_an_application_cache_namespace() {
    let sources = [
        include_str!("derived_word_store/persistent.rs"),
        include_str!("live_capture_store/repository_native.rs"),
    ];
    for token in [
        "default_cache_directory",
        "default_capture_session_directory",
        ".join(\"dsl\")",
    ] {
        assert!(
            sources.iter().all(|source| !source.contains(token)),
            "generic storage source contains application cache policy {token:?}"
        );
    }
}

#[test]
fn application_manager_is_a_facade_instead_of_a_target_dependent_alias() {
    let library = include_str!("lib.rs");
    assert!(!library.contains("type AppManager"));

    for implementation in [
        include_str!("app_manager/native.rs"),
        include_str!("app_manager/wasm.rs"),
    ] {
        assert!(implementation.contains("pub struct AppManager"));
        for operation in [
            "add_node_deferred",
            "start_all_deferred",
            "reconfigure_at",
            "restart_node",
            "request_stop",
            "pump",
        ] {
            assert!(
                implementation.contains(&format!("fn {operation}")),
                "AppManager backend is missing {operation}"
            );
        }
    }
}
