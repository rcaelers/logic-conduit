use clap::Parser;

use logic_analyzer_ui::{APPLICATION_ID, APPLICATION_NAME};

const APPLICATION_LOG_TARGETS: &[&str] = &[
    "logic_conduit",
    "logic_analyzer_ui",
    "logic_analyzer_graph",
    "logic_analyzer_processing",
    "logic_analyzer_viewer",
    "node_graph",
    "panel_layout",
    "trigger_editor",
    "input_bindings",
    "signal_processing",
];

/// Expands the public `logic_conduit` logging namespace to the workspace's
/// local tracing targets.
fn expand_application_log_directives(directives: &str) -> String {
    directives
        .split(',')
        .flat_map(|directive| {
            let directive = directive.trim();
            let Some((target, filter)) = directive.split_once('=') else {
                return vec![directive.to_owned()];
            };
            if target == "logic_conduit" {
                return APPLICATION_LOG_TARGETS
                    .iter()
                    .map(|target| format!("{target}={filter}"))
                    .collect();
            }
            let subsystem = target.strip_prefix("logic_conduit.");
            if let Some(target) = subsystem
                && APPLICATION_LOG_TARGETS.contains(&target)
            {
                return vec![format!("{target}={filter}")];
            }
            vec![directive.to_owned()]
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn application_env_filter() -> tracing_subscriber::EnvFilter {
    let Ok(directives) = std::env::var("RUST_LOG") else {
        return tracing_subscriber::EnvFilter::from_default_env();
    };
    let directives = expand_application_log_directives(&directives);
    tracing_subscriber::EnvFilter::try_new(directives).unwrap_or_else(|error| {
        eprintln!("invalid RUST_LOG filter: {error}");
        tracing_subscriber::EnvFilter::from_default_env()
    })
}

#[cfg(target_os = "macos")]
use crate::macos_menu;

#[derive(Parser)]
#[command(version, about = APPLICATION_NAME)]
struct Args {
    /// Graph JSON file to load at startup
    file: Option<std::path::PathBuf>,
}

pub(crate) type MainResult = eframe::Result;

fn application_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!(
        "../../../resources/icons/LogicConduit.iconset/icon_256x256.png"
    ))
    .expect("embedded LogicConduit application icon is valid PNG")
}

fn link_compile_time_inventories() {
    std::hint::black_box(logic_analyzer_graph_nodes::link());
    #[cfg(feature = "example-plugin")]
    std::hint::black_box(example_plugin::link());
}

pub(crate) fn run() -> MainResult {
    link_compile_time_inventories();
    tracing_subscriber::fmt()
        .with_env_filter(application_env_filter())
        .init();

    let args = Args::parse();
    #[cfg(target_os = "macos")]
    macos_menu::disable_automatic_window_tabbing();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(APPLICATION_ID)
            .with_icon(application_icon())
            .with_inner_size([2100.0, 1350.0])
            .with_title(APPLICATION_NAME),
        ..Default::default()
    };
    eframe::run_native(
        APPLICATION_NAME,
        options,
        Box::new(move |cc| {
            let app = logic_analyzer_ui::App::new_with_file(cc, args.file.as_deref());
            #[cfg(target_os = "macos")]
            macos_menu::install(app.recent_files());
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod logging_tests {
    use super::expand_application_log_directives;

    #[test]
    fn expands_the_application_root_filter_to_workspace_targets() {
        let directives = expand_application_log_directives("logic_conduit=debug");

        assert!(directives.contains("logic_analyzer_processing=debug"));
        assert!(directives.contains("signal_processing=debug"));
    }

    #[test]
    fn expands_an_application_subsystem_filter_to_its_local_target() {
        assert_eq!(
            expand_application_log_directives(
                "logic_conduit.logic_analyzer_processing=debug"
            ),
            "logic_analyzer_processing=debug"
        );
    }

    #[test]
    fn retains_non_application_directives() {
        assert_eq!(
            expand_application_log_directives("warn,eframe=info,logic_conduit=debug"),
            "warn,eframe=info,logic_conduit=debug,logic_analyzer_ui=debug,logic_analyzer_graph=debug,logic_analyzer_processing=debug,logic_analyzer_viewer=debug,node_graph=debug,panel_layout=debug,trigger_editor=debug,input_bindings=debug,signal_processing=debug"
        );
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "example-plugin")]
    #[test]
    fn enabled_plugin_link_makes_its_inventories_visible_to_the_native_host() {
        link_compile_time_inventories();

        let compiler = logic_analyzer_graph::host::GraphCompiler::new();
        let nodes = compiler.build_node_registry();
        assert_eq!(nodes.category_of("Pulse Measure"), Some("Plugin"));
        assert_eq!(nodes.category_of("Camera Frame Source"), Some("Plugin"));

        assert!(
            compiler
                .collected_payloads()
                .descriptor_by_stable_id("org.logicconduit.example.camera-frame/v1")
                .is_some()
        );
    }

    #[test]
    fn built_in_link_makes_node_and_payload_inventories_visible() {
        link_compile_time_inventories();

        let compiler = logic_analyzer_graph::host::GraphCompiler::new();
        let nodes = compiler.build_node_registry();
        assert_eq!(nodes.category_of("SPI Decoder"), Some("Decoders"));
        assert!(
            compiler
                .collected_payloads()
                .descriptor_by_stable_id("org.logicconduit.word/v1")
                .is_some()
        );
    }

    #[test]
    fn embedded_application_icon_is_available() {
        let icon = application_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }

    #[test]
    fn accepts_optional_startup_file() {
        let empty = Args::try_parse_from(["logic-conduit"]).unwrap();
        assert!(empty.file.is_none());

        let with_file = Args::try_parse_from(["logic-conduit", "pipeline.json"]).unwrap();
        assert_eq!(
            with_file.file.as_deref(),
            Some(std::path::Path::new("pipeline.json"))
        );
    }

}
