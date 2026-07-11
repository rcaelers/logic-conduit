#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_arch = "wasm32"))]
use clap::Parser;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Parser)]
#[command(version, about = "DSL Pipeline Editor")]
struct Args {
    /// Graph JSON file to load at startup
    file: Option<std::path::PathBuf>,
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([2100.0, 1350.0])
            .with_title("DSL Pipeline Editor"),
        ..Default::default()
    };
    eframe::run_native(
        "DSL Pipeline Editor",
        options,
        Box::new(move |cc| {
            Ok(Box::new(dsl_ui::App::new_with_plugins_and_file(
                cc,
                args.file.as_deref(),
                |_ctx| {
                    #[cfg(feature = "example-plugin")]
                    example_plugin::register(_ctx);
                },
            )))
        }),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn accepts_optional_startup_file() {
        let empty = Args::try_parse_from(["dsl-ui"]).unwrap();
        assert!(empty.file.is_none());

        let with_file = Args::try_parse_from(["dsl-ui", "examples/ccd_pipeline.json"]).unwrap();
        assert_eq!(
            with_file.file.as_deref(),
            Some(std::path::Path::new("examples/ccd_pipeline.json"))
        );
    }
}
