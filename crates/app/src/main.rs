#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([2100.0, 1350.0])
            .with_title("DSL Pipeline Editor"),
        ..Default::default()
    };
    eframe::run_native(
        "DSL Pipeline Editor",
        options,
        Box::new(|cc| Ok(Box::new(dsl_ui::App::new(cc)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {}
