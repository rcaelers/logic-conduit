use std::path::Path;

use logic_analyzer_graph::{compiler, nodes};
use logic_analyzer_viewer::LogicAnalyzerViewer;
use node_graph::{NodeBadge, NodeGraphWidget, NodeId};

use crate::about::AboutWindow;
use crate::demo_signals;
use crate::toast::Toasts;

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "app_platform/wasm_font.rs"]
        mod font_platform;
        #[path = "app_platform/wasm_hooks.rs"]
        mod platform_hooks;
    }
    _ => {
        #[path = "app_platform/native_font.rs"]
        mod font_platform;
        #[path = "app_platform/native_hooks.rs"]
        mod platform_hooks;
    }
}

use self::font_platform::load_symbol_fonts;

pub struct App {
    node_graph: NodeGraphWidget,
    logic_analyzer: LogicAnalyzerViewer,
    analyzer_split: f32,
    builders: compiler::BuilderRegistry,
    run: Option<compiler::AppRun>,
    /// Persistent run *state* shown in the toolbar next to Run/Stop — the
    /// current compile-error summary, or "stop & rerun to apply" while a
    /// live edit can't be applied in place. One-off events (a live edit that
    /// *did* apply, one that failed) go through `toasts` instead (Phase 4.2).
    run_message: Option<(String, bool /* is_error */)>,
    /// Transient one-off notifications (file loaded/saved, node(s)
    /// copied/pasted, live-edit results) — bottom-right, self-clearing.
    toasts: Toasts,
    platform: crate::app_platform::PlatformState,
    about: AboutWindow,
    /// Nodes badged with compile errors; cleared on the next Run.
    error_badges: Vec<NodeId>,
    /// Last time the running pipeline was diffed against the edited graph.
    last_live_sync: f64,
}

impl App {
    fn set_capture_preview(&mut self, signals: Vec<nodes::CapturePreviewSignal>) {
        let duration_us = signals
            .iter()
            .flat_map(|signal| signal.transitions.last().map(|(time, _)| *time))
            .fold(1.0_f64, f64::max);
        let channels = signals
            .into_iter()
            .map(|signal| logic_analyzer_viewer::ChannelSignal {
                index: signal.index,
                name: signal.name,
                initial: signal.initial,
                transitions: signal.transitions,
            })
            .collect();
        self.logic_analyzer
            .set_channels_with_duration(channels, duration_us);
    }

    pub fn new(cc: &eframe::CreationContext) -> Self {
        Self::new_with_plugins(cc, |_ctx| {})
    }

    /// The persisted MRU list, most recent first — read once at startup by
    /// the native macOS menu to build its "Open Recent" submenu (Phase 5.1).
    /// Empty on wasm, where there is no recent-files list at all.
    pub fn recent_files(&self) -> &[std::path::PathBuf] {
        self.platform.recent_files()
    }

    /// Like [`Self::new`], but first runs `register_plugins` against a
    /// [`compiler::PluginContext`] wrapping the freshly built registries.
    /// This is the hook a downstream crate (e.g. `logic-analyzer-app-native`) uses to link in
    /// compile-time plugin crates — `logic-analyzer-ui` itself never depends on any
    /// plugin (a plugin depends on `logic-analyzer-graph`, so the reverse would be a
    /// dependency cycle), so the actual `example_plugin::register(...)`
    /// call lives at the binary crate that depends on both.
    pub fn new_with_plugins(
        cc: &eframe::CreationContext,
        register_plugins: impl FnOnce(&mut compiler::PluginContext),
    ) -> Self {
        Self::new_with_plugins_and_file(cc, None, register_plugins)
    }

    pub fn new_with_plugins_and_file(
        cc: &eframe::CreationContext,
        file: Option<&Path>,
        register_plugins: impl FnOnce(&mut compiler::PluginContext),
    ) -> Self {
        let mut app = Self::build(cc, register_plugins);
        app.platform_load_startup_file(file);
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
        let mut widget = NodeGraphWidget::new(registry);
        let (platform, analyzer_split) =
            crate::app_platform::PlatformState::restore(cc, &mut widget);
        let mut logic_analyzer = LogicAnalyzerViewer::new();
        logic_analyzer.set_channels(demo_signals::channels());
        Self {
            node_graph: widget,
            logic_analyzer,
            analyzer_split,
            builders,
            run: None,
            run_message: None,
            toasts: Toasts::default(),
            platform,
            about: AboutWindow::new(),
            error_badges: Vec::new(),
            last_live_sync: 0.0,
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

        // Fresh lane store per run: stale lanes vanish atomically.
        let mut ctx = compiler::CompileCtx::default();
        self.platform_prepare_run(&mut ctx);
        self.logic_analyzer
            .set_derived_lanes(ctx.derived_lanes.clone());
        self.logic_analyzer
            .set_viewer_lanes(ctx.viewer_lanes.clone());

        match compiler::start_app_run(self.node_graph.graph(), &self.builders, &mut ctx) {
            Ok(run) => {
                self.run = Some(run);
            }
            Err(errors) => self.report_compile_errors(&errors),
        }
    }

    fn is_running(&self) -> bool {
        self.run.as_ref().is_some_and(|run| !run.is_finished())
    }

    fn is_stopping(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.is_stopping())
    }

    /// Run/Stop menu items and their `Cmd+R`/`Cmd+.` accelerators (Phase
    /// 5.3) — guarded the same way the toolbar's own Run/Stop buttons
    /// already are (only one is ever shown at a time), so triggering either
    /// while it doesn't apply (Run while already running, Stop while not)
    /// is a safe no-op rather than double-starting or double-stopping.
    fn run_command(&mut self) {
        if !self.is_running() {
            self.start_run();
        }
    }

    fn stop_command(&mut self) {
        if self.is_running()
            && !self.is_stopping()
            && let Some(run) = &mut self.run
        {
            run.stop();
        }
    }

    /// Drives the run forward and, periodically, diffs the edited graph
    /// against it and applies what can be applied live (`docs/APP_DESIGN.md`,
    /// live editing): taps, branch
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

        // Per-node progress in the headers — also after the
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
                self.toasts.info(format!(
                    "live: +{} −{} cfg {} restart {}",
                    summary.added, summary.removed, summary.configured, summary.restarted
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
                self.toasts.error(format!("live edit failed: {message}"));
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

    fn show_toolbar(&mut self, ui: &mut egui::Ui, status_hint: &str) {
        self.sync_run(ui.ctx());
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let running = self.is_running();
            let stopping = self.is_stopping();
            if running && stopping {
                // Wind-down signalled; threads are finishing their current
                // work. Nothing to click — is_finished() flips shortly.
                ui.spinner();
                ui.label("Stopping…");
            } else if running {
                if ui.button("⏹ Stop").clicked() {
                    self.stop_command();
                }
                ui.spinner();
                ui.label("Live");
            } else {
                if ui.button("▶ Run").clicked() {
                    self.run_command();
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

            // Right-aligned: `<hint> | <zoom%> <selection>`, reading left to
            // right. `right_to_left` places each widget to the left of the
            // previous one, so they're added in reverse of that order —
            // `selection_summary` ends up flush with the right edge.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if ui.small_button("About").clicked() {
                    self.about.open();
                }
                ui.weak(self.node_graph.selection_summary());
                ui.weak(format!("{}%", self.node_graph.zoom_percent()));
                ui.separator();
                if !status_hint.is_empty() {
                    ui.weak(status_hint);
                }
            });
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

/// Adds the platform's native symbol fonts as fallbacks for menu icon glyphs
/// that egui's bundled fonts don't cover.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (index, font_data) in load_symbol_fonts().into_iter().enumerate() {
        let font_name = format!("system-symbols-{index}");
        fonts
            .font_data
            .insert(font_name.clone(), std::sync::Arc::new(font_data));
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .push(font_name);
    }
    ctx.set_fonts(fonts);
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.platform_raw_input_hook(ctx, raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.platform_logic(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.platform_save(storage);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.platform_before_ui(ui);

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

        self.platform_sync_capture();

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

        // Which pane's status hint the toolbar shows (Phase 4.1) — computed
        // from plain rects rather than this frame's widget hover state,
        // since the analyzer and graph haven't rendered yet this frame.
        let pointer_pos = ui.input(|i| i.pointer.hover_pos());
        let analyzer_rect =
            egui::Rect::from_min_size(origin, egui::vec2(available.x, analyzer_height));
        let graph_top = origin.y + analyzer_height + splitter_hit_height + toolbar_height;
        let graph_rect = egui::Rect::from_min_size(
            egui::pos2(origin.x, graph_top),
            egui::vec2(available.x, graph_height),
        );
        let status_hint = match pointer_pos {
            Some(p) if analyzer_rect.contains(p) => self.logic_analyzer.status_hint(),
            Some(p) if graph_rect.contains(p) => self.node_graph.status_hint(),
            _ => "",
        };

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
            self.show_toolbar(ui, status_hint);
        });

        self.platform_before_graph();
        ui.allocate_ui(egui::vec2(available.x, graph_height), |ui| {
            self.node_graph.show(ui);
        });
        if let Some(message) = self.node_graph.take_io_status() {
            self.toasts.info(message);
        }
        self.platform_after_graph();

        self.about.show(ui.ctx());

        self.platform_after_ui(ui.ctx());

        self.toasts.show(ui.ctx());
    }
}

#[cfg(test)]
mod font_tests {
    use super::{install_fonts, load_symbol_fonts};

    #[test]
    fn menu_icon_glyphs_are_available() {
        assert!(
            !load_symbol_fonts().is_empty(),
            "missing platform symbol font; expected Apple Symbols on macOS, Segoe UI Symbol on Windows, or Noto Sans Symbols, Symbols 2, and Math on Linux"
        );
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        #[cfg(debug_assertions)]
        assert!(
            ctx.style_of(egui::Theme::Dark)
                .debug
                .warn_if_rect_changes_id
        );
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
