use crate::logic_analyzer_viewer::LogicAnalyzerViewer;
use crate::nodes;
use node_graph::NodeGraphWidget;

pub struct App {
    node_graph: NodeGraphWidget,
    logic_analyzer: LogicAnalyzerViewer,
    analyzer_split: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        install_fonts(&cc.egui_ctx);
        let registry = nodes::build_registry();
        let mut widget = NodeGraphWidget::new(registry);
        nodes::populate_startup(&mut widget);
        Self {
            node_graph: widget,
            logic_analyzer: LogicAnalyzerViewer::demo(),
            analyzer_split: 0.42,
        }
    }
}

/// Adds the platform's native symbol font as a fallback for menu icon glyphs
/// that egui's bundled fonts don't cover.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(font_data) = load_symbol_font() {
        const FONT_NAME: &str = "system-symbols";
        fonts
            .font_data
            .insert(FONT_NAME.to_owned(), std::sync::Arc::new(font_data));
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .push(FONT_NAME.to_owned());
    }
    ctx.set_fonts(fonts);
}

fn load_symbol_font() -> Option<egui::FontData> {
    symbol_font_paths()
        .iter()
        .find_map(|path| std::fs::read(path).ok())
        .map(egui::FontData::from_owned)
}

#[cfg(target_os = "macos")]
fn symbol_font_paths() -> &'static [&'static str] {
    &["/System/Library/Fonts/Apple Symbols.ttf"]
}

#[cfg(target_os = "windows")]
fn symbol_font_paths() -> &'static [&'static str] {
    &[r"C:\Windows\Fonts\seguisym.ttf"]
}

#[cfg(target_os = "linux")]
fn symbol_font_paths() -> &'static [&'static str] {
    &[
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/google-noto-sans-symbols2-fonts/NotoSansSymbols2-Regular.ttf",
        "/usr/local/share/fonts/NotoSansSymbols2-Regular.ttf",
    ]
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn symbol_font_paths() -> &'static [&'static str] {
    &[]
}

#[cfg(test)]
mod font_tests {
    use super::{install_fonts, load_symbol_font};

    #[test]
    fn menu_icon_glyphs_are_available() {
        assert!(
            load_symbol_font().is_some(),
            "missing platform symbol font; expected Apple Symbols on macOS, Segoe UI Symbol on Windows, or Noto Sans Symbols 2 on Linux"
        );
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        // `set_fonts` only takes effect at the start of the *next* pass.
        ctx.begin_pass(Default::default());
        let _ = ctx.end_pass();
        ctx.begin_pass(Default::default());
        let font_id = egui::FontId::proportional(14.0);
        ctx.fonts_mut(|fonts| {
            const MENU_GLYPHS: &[char] = &['⇧', '⌘', '⌥', '⇪', '⏎', '↶', '↷', '⌧', '⎘', '⧉', '▣'];
            for c in MENU_GLYPHS {
                assert!(
                    fonts.has_glyph(&font_id, *c),
                    "missing glyph for {c:?} (U+{:04X})",
                    *c as u32
                );
            }
        });
        let _ = ctx.end_pass();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let available = ui.available_size();
        let splitter_hit_height = 7.0;
        let splitter_visual_height = 2.0;
        let usable_height = (available.y - splitter_hit_height).max(0.0);
        let analyzer_min = 160.0;
        let graph_min = 160.0;
        let mut analyzer_height = usable_height * self.analyzer_split;
        if usable_height >= analyzer_min + graph_min {
            analyzer_height = analyzer_height.clamp(analyzer_min, usable_height - graph_min);
        }

        if let Some(file) = nodes::dsl_file_source_path(self.node_graph.graph()) {
            self.logic_analyzer.set_capture_path(file);
        }

        let origin = ui.cursor().min;
        let splitter_rect = egui::Rect::from_min_size(
            egui::pos2(origin.x, origin.y + analyzer_height),
            egui::vec2(available.x, splitter_hit_height),
        );
        let splitter_id = ui.id().with("logic_analyzer_node_graph_splitter");
        let splitter_response =
            ui.interact(splitter_rect, splitter_id, egui::Sense::click_and_drag());
        if splitter_response.hovered() || splitter_response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if splitter_response.dragged() && usable_height > 0.0 {
            analyzer_height = (splitter_response
                .interact_pointer_pos()
                .map_or(analyzer_height, |pos| {
                    pos.y - origin.y - splitter_hit_height * 0.5
                }))
            .clamp(0.0, usable_height);
            if usable_height >= analyzer_min + graph_min {
                analyzer_height = analyzer_height.clamp(analyzer_min, usable_height - graph_min);
            }
            self.analyzer_split = (analyzer_height / usable_height).clamp(0.05, 0.95);
        }
        let graph_height = (usable_height - analyzer_height).max(0.0);

        ui.allocate_ui(egui::vec2(available.x, analyzer_height), |ui| {
            self.logic_analyzer.show(ui);
        });

        ui.allocate_space(egui::vec2(available.x, splitter_hit_height));
        let splitter_color = if splitter_response.dragged() || splitter_response.hovered() {
            egui::Color32::from_rgb(90, 90, 90)
        } else {
            egui::Color32::from_rgb(58, 58, 58)
        };
        let visual_rect = egui::Rect::from_center_size(
            splitter_rect.center(),
            egui::vec2(splitter_rect.width(), splitter_visual_height),
        );
        ui.painter().rect_filled(visual_rect, 0.0, splitter_color);

        ui.allocate_ui(egui::vec2(available.x, graph_height), |ui| {
            self.node_graph.show(ui);
        });
    }
}
