#[cfg(not(target_arch = "wasm32"))]
use crate::compile;
use crate::logic_analyzer_viewer::LogicAnalyzerViewer;
use crate::nodes;
use node_graph::NodeGraphWidget;
#[cfg(not(target_arch = "wasm32"))]
use node_graph::{NodeBadge, NodeId};

pub struct App {
    node_graph: NodeGraphWidget,
    logic_analyzer: LogicAnalyzerViewer,
    analyzer_split: f32,
    #[cfg(not(target_arch = "wasm32"))]
    builders: compile::BuilderRegistry,
    #[cfg(not(target_arch = "wasm32"))]
    run: Option<compile::LiveRun>,
    /// Last global compile/run message shown in the toolbar.
    run_message: Option<(String, bool /* is_error */)>,
    /// Nodes badged with compile errors; cleared on the next Run.
    #[cfg(not(target_arch = "wasm32"))]
    error_badges: Vec<NodeId>,
    /// Last time the running pipeline was diffed against the edited graph.
    #[cfg(not(target_arch = "wasm32"))]
    last_live_sync: f64,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        install_fonts(&cc.egui_ctx);
        let registry = nodes::build_registry();
        let mut widget = NodeGraphWidget::new(registry);
        #[cfg(not(target_arch = "wasm32"))]
        nodes::populate_startup(&mut widget);
        #[cfg(target_arch = "wasm32")]
        nodes::populate_uart_demo(&mut widget);
        Self {
            node_graph: widget,
            logic_analyzer: LogicAnalyzerViewer::demo(),
            analyzer_split: 0.42,
            #[cfg(not(target_arch = "wasm32"))]
            builders: compile::BuilderRegistry::standard(),
            #[cfg(not(target_arch = "wasm32"))]
            run: None,
            run_message: None,
            #[cfg(not(target_arch = "wasm32"))]
            error_badges: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            last_live_sync: 0.0,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn report_compile_errors(&mut self, errors: &[compile::CompileError]) {
        for error in errors {
            if let Some(id) = error.node {
                self.node_graph
                    .set_node_badge(id, Some(NodeBadge::error(&error.message)));
                self.error_badges.push(id);
            }
        }
        let summary = errors
            .first()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| "compile failed".to_owned());
        let extra = errors.len().saturating_sub(1);
        self.run_message = Some((
            if extra > 0 {
                format!("{summary} (+{extra} more)")
            } else {
                summary
            },
            true,
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn start_run(&mut self) {
        for id in self.error_badges.drain(..) {
            self.node_graph.set_node_badge(id, None);
        }
        self.node_graph.clear_node_statuses();
        self.run_message = None;

        // Fresh lane store per run: stale lanes vanish atomically (§5.5).
        let mut ctx = compile::CompileCtx::default();
        self.logic_analyzer
            .set_derived_lanes(ctx.derived_lanes.clone());

        match compile::start_live(self.node_graph.graph(), &self.builders, &mut ctx) {
            Ok(run) => {
                self.run = Some(run);
            }
            Err(errors) => self.report_compile_errors(&errors),
        }
    }

    /// While running, periodically diff the edited graph against the live
    /// pipeline and apply what can be applied (§6.5): taps, branch
    /// removals, hot prop changes, in-place restarts. Edits that need a
    /// full restart leave the run untouched and say so.
    #[cfg(not(target_arch = "wasm32"))]
    fn sync_live_edits(&mut self, ctx: &egui::Context) {
        const SYNC_INTERVAL_S: f64 = 0.5;
        let Some(run) = &mut self.run else {
            return;
        };
        let now = ctx.input(|input| input.time);
        if now - self.last_live_sync < SYNC_INTERVAL_S {
            return;
        }
        self.last_live_sync = now;

        // Per-node progress in the headers (§7 Phase 6) — also after the
        // run finished, so the final counts stick.
        for (id, items) in run.progress() {
            let status = (items > 0).then(|| format_count(items));
            self.node_graph.set_node_status(id, status);
        }
        let Some(run) = &mut self.run else {
            return;
        };
        if run.is_finished() {
            return;
        }

        match run.apply(self.node_graph.graph(), &self.builders) {
            Ok(summary) if summary.is_empty() => {}
            Ok(summary) => {
                self.run_message = Some((
                    format!(
                        "live: +{} −{} cfg {} restart {}",
                        summary.added, summary.removed, summary.configured, summary.restarted
                    ),
                    false,
                ));
            }
            Err(compile::ApplyError::Compile(_)) => {
                // Mid-edit graphs are often momentarily invalid; keep the
                // running pipeline and wait for the graph to become valid.
            }
            Err(compile::ApplyError::NeedsFullRestart(reason)) => {
                self.run_message = Some((format!("stop & rerun to apply: {reason}"), false));
            }
            Err(compile::ApplyError::Apply(message)) => {
                self.run_message = Some((format!("live edit failed: {message}"), true));
            }
        }

        for (node, event) in run.take_disconnected() {
            if let Some(id) = node {
                self.node_graph.set_node_badge(
                    id,
                    Some(NodeBadge::warning(format!(
                        "Disconnected: can't keep up with {}.{}",
                        event.producer, event.port
                    ))),
                );
                self.error_badges.push(id);
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn show_toolbar(&mut self, ui: &mut egui::Ui) {
        self.sync_live_edits(ui.ctx());
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let running = self.run.as_ref().is_some_and(|run| !run.is_finished());
            if running {
                if ui.button("⏹ Stop").clicked() {
                    if let Some(run) = &mut self.run {
                        run.stop();
                    }
                }
                ui.spinner();
                ui.label("Live");
                // Keep polling the background run without user input.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
            } else {
                if ui.button("▶ Run").clicked() {
                    self.start_run();
                }
                if self.run.is_some() {
                    ui.label("Finished");
                }
            }
            if let Some((message, is_error)) = &self.run_message {
                let color = if *is_error {
                    egui::Color32::from_rgb(230, 120, 120)
                } else {
                    egui::Color32::from_rgb(180, 180, 180)
                };
                ui.colored_label(color, message);
            }
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn show_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label("Web demo");
            ui.colored_label(
                egui::Color32::from_rgb(180, 180, 180),
                "native capture and pipeline execution are disabled in this build",
            );
            if let Some((message, is_error)) = &self.run_message {
                let color = if *is_error {
                    egui::Color32::from_rgb(230, 120, 120)
                } else {
                    egui::Color32::from_rgb(180, 180, 180)
                };
                ui.colored_label(color, message);
            }
        });
    }
}

/// Compact item-count formatting for node headers: 950 → "950", 12_345 →
/// "12.3k", 5_600_000 → "5.6M", 2_100_000_000 → "2.1G".
fn format_count(items: u64) -> String {
    match items {
        0..=999 => items.to_string(),
        1_000..=999_999 => format!("{:.1}k", items as f64 / 1_000.0),
        1_000_000..=999_999_999 => format!("{:.1}M", items as f64 / 1_000_000.0),
        _ => format!("{:.1}G", items as f64 / 1_000_000_000.0),
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

#[cfg(target_arch = "wasm32")]
fn load_symbol_font() -> Option<egui::FontData> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_arch = "wasm32"
)))]
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
        let toolbar_height = 28.0;
        let usable_height = (available.y - splitter_hit_height - toolbar_height).max(0.0);
        let analyzer_min = 160.0;
        let graph_min = 160.0;
        let mut analyzer_height = usable_height * self.analyzer_split;
        if usable_height >= analyzer_min + graph_min {
            analyzer_height = analyzer_height.clamp(analyzer_min, usable_height - graph_min);
        }

        #[cfg(not(target_arch = "wasm32"))]
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

        ui.allocate_ui(egui::vec2(available.x, toolbar_height), |ui| {
            self.show_toolbar(ui);
        });

        ui.allocate_ui(egui::vec2(available.x, graph_height), |ui| {
            self.node_graph.show(ui);
        });
    }
}
