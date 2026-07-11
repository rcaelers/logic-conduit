use crate::compiler;
use crate::demo_signals;
use crate::nodes;
use logic_analyzer_viewer::LogicAnalyzerViewer;
use node_graph::{NodeBadge, NodeGraphWidget, NodeId};
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

#[cfg(not(target_arch = "wasm32"))]
enum FileCommand {
    Load,
    Save,
    Quit,
}

pub struct App {
    node_graph: NodeGraphWidget,
    logic_analyzer: LogicAnalyzerViewer,
    analyzer_split: f32,
    builders: compiler::BuilderRegistry,
    run: Option<compiler::AppRun>,
    /// Last global compile/run message shown in the toolbar.
    run_message: Option<(String, bool /* is_error */)>,
    /// Last document load/save message shown in the toolbar.
    file_message: Option<(String, bool /* is_error */)>,
    #[cfg(not(target_arch = "wasm32"))]
    current_file: Option<PathBuf>,
    /// Nodes badged with compile errors; cleared on the next Run.
    error_badges: Vec<NodeId>,
    /// Last time the running pipeline was diffed against the edited graph.
    last_live_sync: f64,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        Self::new_with_plugins(cc, |_ctx| {})
    }

    /// Like [`Self::new`], but first runs `register_plugins` against a
    /// [`compiler::PluginContext`] wrapping the freshly built registries.
    /// This is the hook a downstream crate (e.g. `dsl-app`) uses to link in
    /// compile-time plugin crates — `dsl-ui` itself never depends on any
    /// plugin (a plugin depends on `dsl-ui`, so the reverse would be a
    /// dependency cycle), so the actual `example_plugin::register(...)`
    /// call lives at the binary crate that depends on both.
    pub fn new_with_plugins(
        cc: &eframe::CreationContext,
        register_plugins: impl FnOnce(&mut compiler::PluginContext),
    ) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        return Self::new_with_plugins_and_file(cc, None, register_plugins);

        #[cfg(target_arch = "wasm32")]
        Self::build(cc, register_plugins)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_with_plugins_and_file(
        cc: &eframe::CreationContext,
        file: Option<&Path>,
        register_plugins: impl FnOnce(&mut compiler::PluginContext),
    ) -> Self {
        let mut app = Self::build(cc, register_plugins);
        if let Some(file) = file {
            app.load_file(file.to_owned());
        }
        app
    }

    fn build(
        cc: &eframe::CreationContext,
        register_plugins: impl FnOnce(&mut compiler::PluginContext),
    ) -> Self {
        install_fonts(&cc.egui_ctx);
        let mut registry = nodes::build_registry();
        let mut builders = compiler::BuilderRegistry::standard();
        register_plugins(&mut compiler::PluginContext::new(
            &mut registry,
            &mut builders,
        ));
        #[allow(unused_mut)] // mutable only for the wasm demo graph
        let mut widget = NodeGraphWidget::new(registry);
        #[cfg(target_arch = "wasm32")]
        nodes::populate_uart_demo(&mut widget);
        let mut logic_analyzer = LogicAnalyzerViewer::new();
        logic_analyzer.set_channels(demo_signals::channels());
        Self {
            node_graph: widget,
            logic_analyzer,
            analyzer_split: 0.42,
            builders,
            run: None,
            run_message: None,
            file_message: None,
            #[cfg(not(target_arch = "wasm32"))]
            current_file: None,
            error_badges: Vec::new(),
            last_live_sync: 0.0,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load_file(&mut self, path: PathBuf) {
        match self.node_graph.load_from_path(&path) {
            Ok(()) => {
                if let Some(run) = &mut self.run {
                    run.stop();
                }
                self.run_message = None;
                self.error_badges.clear();
                self.current_file = Some(path.clone());
                self.file_message = Some((format!("Loaded {}", path.display()), false));
            }
            Err(error) => self.file_message = Some((error, true)),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn choose_and_load_file(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Load graph")
            .add_filter("Graph JSON", &["json"]);
        if let Some(parent) = self.current_file.as_ref().and_then(|path| path.parent()) {
            dialog = dialog.set_directory(parent);
        }
        if let Some(path) = dialog.pick_file() {
            self.load_file(path);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_file(&mut self) {
        let path = self.current_file.clone().or_else(|| {
            rfd::FileDialog::new()
                .set_title("Save graph")
                .set_file_name("pipeline.json")
                .add_filter("Graph JSON", &["json"])
                .save_file()
        });
        let Some(path) = path else {
            return;
        };
        match self.node_graph.save_to_path(&path) {
            Ok(()) => {
                self.current_file = Some(path.clone());
                self.file_message = Some((format!("Saved {}", path.display()), false));
            }
            Err(error) => self.file_message = Some((error, true)),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn show_menu_bar(&mut self, ui: &mut egui::Ui) {
        let load_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O);
        let save_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S);
        let mut command = if ui.input_mut(|input| input.consume_shortcut(&load_shortcut)) {
            Some(FileCommand::Load)
        } else if ui.input_mut(|input| input.consume_shortcut(&save_shortcut)) {
            Some(FileCommand::Save)
        } else {
            None
        };

        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add(
                        egui::Button::new("Load...")
                            .shortcut_text(ui.ctx().format_shortcut(&load_shortcut)),
                    )
                    .clicked()
                {
                    command = Some(FileCommand::Load);
                    ui.close();
                }
                if ui
                    .add(
                        egui::Button::new("Save")
                            .shortcut_text(ui.ctx().format_shortcut(&save_shortcut)),
                    )
                    .clicked()
                {
                    command = Some(FileCommand::Save);
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    command = Some(FileCommand::Quit);
                    ui.close();
                }
            });
        });

        match command {
            Some(FileCommand::Load) => self.choose_and_load_file(),
            Some(FileCommand::Save) => self.save_file(),
            Some(FileCommand::Quit) => ui.send_viewport_cmd(egui::ViewportCommand::Close),
            None => {}
        }
    }

    fn report_compile_errors(&mut self, errors: &[compiler::CompileError]) {
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

    fn start_run(&mut self) {
        for id in self.error_badges.drain(..) {
            self.node_graph.set_node_badge(id, None);
        }
        self.node_graph.clear_node_statuses();
        self.run_message = None;

        // Fresh lane store per run: stale lanes vanish atomically (§5.5).
        let mut ctx = compiler::CompileCtx::default();
        self.logic_analyzer
            .set_derived_lanes(ctx.derived_lanes.clone());

        match compiler::start_app_run(self.node_graph.graph(), &self.builders, &mut ctx) {
            Ok(run) => {
                self.run = Some(run);
            }
            Err(errors) => self.report_compile_errors(&errors),
        }
    }

    /// Drives the run forward and, periodically, diffs the edited graph
    /// against it and applies what can be applied live (§6.5): taps, branch
    /// removals, hot prop changes, in-place restarts. Edits that need a full
    /// restart leave the run untouched and say so.
    ///
    /// `pump()` is called every frame — a no-op on the native threaded
    /// manager (its nodes run themselves in the background), but on wasm's
    /// cooperative manager it's what actually executes node `work()`, so it
    /// can't be gated behind the same throttle as the `apply()` diff below.
    fn sync_run(&mut self, ctx: &egui::Context) {
        const SYNC_INTERVAL_S: f64 = 0.5;
        let Some(run) = &mut self.run else {
            return;
        };
        run.pump(256);
        if !run.is_finished() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

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
        // No live edits once the run is done or winding down — `apply()`'s
        // remove/restart paths join node threads, which a stopping run may
        // not finish promptly.
        if run.is_finished() || run.is_stopping() {
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
            Err(compiler::ApplyError::Compile(_)) => {
                // Mid-edit graphs are often momentarily invalid; keep the
                // running pipeline and wait for the graph to become valid.
            }
            Err(compiler::ApplyError::NeedsFullRestart(reason)) => {
                self.run_message = Some((format!("stop & rerun to apply: {reason}"), false));
            }
            Err(compiler::ApplyError::Apply(message)) => {
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

    fn show_toolbar(&mut self, ui: &mut egui::Ui) {
        self.sync_run(ui.ctx());
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let running = self.run.as_ref().is_some_and(|run| !run.is_finished());
            let stopping = self.run.as_ref().is_some_and(|run| run.is_stopping());
            if running && stopping {
                // Wind-down signalled; threads are finishing their current
                // work. Nothing to click — is_finished() flips shortly.
                ui.spinner();
                ui.label("Stopping…");
            } else if running {
                if ui.button("⏹ Stop").clicked()
                    && let Some(run) = &mut self.run
                {
                    run.stop();
                }
                ui.spinner();
                ui.label("Live");
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
            if let Some((message, is_error)) = &self.file_message {
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
        #[cfg(not(target_arch = "wasm32"))]
        self.show_menu_bar(ui);

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

        // The viewer only knows the generic `CaptureDataSource` trait; this
        // is the one place that knows a `.dsl` file path means opening it
        // with `DslFileCaptureDataSource`.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(file) = nodes::dsl_file_source_path(self.node_graph.graph()) {
            self.logic_analyzer.set_capture_path(file, |path| {
                dsl::DslFileCaptureDataSource::open(path).map_err(|e| e.to_string())
            });
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
