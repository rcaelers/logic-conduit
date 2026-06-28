#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod logic_analyzer_viewer;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_title("DSL Pipeline Editor"),
        ..Default::default()
    };
    eframe::run_native(
        "DSL Pipeline Editor",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
