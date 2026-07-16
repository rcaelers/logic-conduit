use std::path::Path;
use std::sync::Arc;

use input_bindings::{InputBindings, PointerButtonName, PointerGesture, Trigger};
use logic_analyzer_graph::{compiler, nodes};
use logic_analyzer_viewer::LogicAnalyzerViewer;
use node_graph::{NodeBadge, NodeGraphWidget, NodeId};
use panel_layout::{PanelSlot, PanelSpec, VerticalPanelLayout};

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
    input_bindings: Arc<InputBindings>,
    panel_layout: VerticalPanelLayout,
    builders: compiler::BuilderRegistry,
    run: Option<compiler::AppRun>,
    /// Persistent run *state* shown in the status bar next to Run/Stop — the
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
        // The graph canvas and its custom widgets use a dark palette. Do not
        // inherit a light OS/browser preference for the surrounding egui
        // controls, or their dark foreground text becomes unreadable there.
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        install_fonts(&cc.egui_ctx);
        let mut registry = nodes::build_registry();
        let input_bindings = Arc::new(crate::application_input_bindings().clone());
        let mut builders = compiler::BuilderRegistry::standard();
        register_plugins(&mut compiler::PluginContext::new(
            &mut registry,
            &mut builders,
        ));
        let mut widget = NodeGraphWidget::new(registry);
        widget.set_input_bindings(input_bindings.clone());
        let (platform, analyzer_split) =
            crate::app_platform::PlatformState::restore(cc, &mut widget);
        let mut logic_analyzer = LogicAnalyzerViewer::new();
        logic_analyzer.set_input_bindings(input_bindings.clone());
        logic_analyzer.set_channels(demo_signals::channels());
        Self {
            node_graph: widget,
            logic_analyzer,
            input_bindings,
            panel_layout: VerticalPanelLayout::new([
                ("logic_analyzer", analyzer_split),
                ("node_graph", 1.0 - analyzer_split),
            ]),
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

    fn show_status_bar(&mut self, ui: &mut egui::Ui, actions: &[StatusAction]) {
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, 0.0, egui::Color32::from_rgb(30, 30, 30));
        ui.painter().line_segment(
            [rect.left_top(), rect.right_top()],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(78, 78, 78)),
        );
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            for action in actions {
                status_input_badge(ui, &action.input);
                ui.weak(action.label.as_str());
                ui.add_space(8.0);
            }

            // Right-aligned: `<zoom%> <selection>`, reading left to
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
            });
        });
    }

    fn show_run_controls(&mut self, ui: &mut egui::Ui) {
        self.sync_run(ui.ctx());
        ui.separator();
        let running = self.is_running();
        let stopping = self.is_stopping();
        if running && stopping {
            // Wind-down signalled; threads are finishing their current work.
            ui.spinner();
            ui.label("Stopping…");
        } else if running {
            if ui.small_button("⏹ Stop").clicked() {
                self.stop_command();
            }
            ui.spinner();
            ui.label("Live");
        } else {
            if ui.small_button("▶ Run").clicked() {
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
    }

    fn status_actions(
        &self,
        viewer_context: Option<&str>,
        over_graph: bool,
        modifiers: egui::Modifiers,
    ) -> Vec<StatusAction> {
        let contexts = if over_graph {
            vec!["node_graph", "global"]
        } else if let Some(viewer_context) = viewer_context {
            vec![viewer_context, "logic_analyzer", "global"]
        } else {
            vec!["global"]
        };
        self.input_bindings
            .status_bindings(&contexts, modifiers)
            .into_iter()
            .filter_map(StatusAction::from_binding)
            .collect()
    }
}

const STATUS_BAR_HEIGHT: f32 = 28.0;

#[derive(Clone, Copy)]
enum MouseButtonHint {
    Left,
    Middle,
    Right,
    Wheel,
}

#[derive(Clone)]
enum StatusInput {
    Mouse {
        button: MouseButtonHint,
        gesture: Option<PointerGesture>,
    },
    Key(String),
}

#[derive(Clone)]
struct StatusAction {
    input: StatusInput,
    label: String,
}

impl StatusAction {
    fn from_binding(binding: &input_bindings::Binding) -> Option<Self> {
        let input = match &binding.trigger {
            Trigger::Pointer { button, gesture } => StatusInput::Mouse {
                button: match button {
                    PointerButtonName::Primary => MouseButtonHint::Left,
                    PointerButtonName::Middle => MouseButtonHint::Middle,
                    PointerButtonName::Secondary => MouseButtonHint::Right,
                    PointerButtonName::Extra1 | PointerButtonName::Extra2 => return None,
                },
                gesture: Some(*gesture),
            },
            Trigger::Wheel { .. } | Trigger::Zoom => StatusInput::Mouse {
                button: MouseButtonHint::Wheel,
                gesture: None,
            },
            Trigger::Key { key } => StatusInput::Key(key_name(key)),
        };
        Some(Self {
            input,
            label: binding.label.clone(),
        })
    }
}

fn key_name(key: &str) -> String {
    match key {
        "arrow_down" => "↓".to_owned(),
        "arrow_left" => "←".to_owned(),
        "arrow_right" => "→".to_owned(),
        "arrow_up" => "↑".to_owned(),
        other if other.len() == 1 => other.to_ascii_uppercase(),
        other => other.replace('_', " "),
    }
}

fn status_input_badge(ui: &mut egui::Ui, input: &StatusInput) {
    match input {
        StatusInput::Mouse { button, gesture } => draw_mouse_badge(ui, *button, *gesture),
        StatusInput::Key(key) => draw_key_badge(ui, key),
    }
}

fn draw_mouse_badge(ui: &mut egui::Ui, button: MouseButtonHint, gesture: Option<PointerGesture>) {
    let double_click = gesture == Some(PointerGesture::DoubleClick);
    let width = if double_click { 38.0 } else { 22.0 };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 22.0), egui::Sense::hover());
    let mouse = egui::Rect::from_min_size(rect.min, egui::vec2(22.0, 22.0)).shrink(1.0);
    let divider_y = mouse.top() + 8.0;
    let fill = egui::Color32::from_rgb(155, 155, 155);
    match button {
        MouseButtonHint::Left => ui.painter().rect_filled(
            egui::Rect::from_min_max(mouse.min, egui::pos2(mouse.center().x, divider_y)),
            3.0,
            fill,
        ),
        MouseButtonHint::Right => ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(mouse.center().x, mouse.top()),
                egui::pos2(mouse.right(), divider_y),
            ),
            3.0,
            fill,
        ),
        MouseButtonHint::Middle | MouseButtonHint::Wheel => ui.painter().rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(mouse.center().x, mouse.top() + 5.0),
                egui::vec2(3.5, 7.0),
            ),
            2.0,
            fill,
        ),
    };
    let stroke = egui::Stroke::new(1.2, egui::Color32::from_rgb(165, 165, 165));
    ui.painter()
        .rect_stroke(mouse, 5.0, stroke, egui::StrokeKind::Inside);
    ui.painter().line_segment(
        [
            egui::pos2(mouse.left(), divider_y),
            egui::pos2(mouse.right(), divider_y),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            egui::pos2(mouse.center().x, mouse.top()),
            egui::pos2(mouse.center().x, divider_y),
        ],
        stroke,
    );
    if double_click {
        ui.painter().text(
            egui::pos2(mouse.right() + 8.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            "2×",
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgb(200, 200, 200),
        );
    }
}

fn draw_key_badge(ui: &mut egui::Ui, key: &str) {
    let width = (key.chars().count() as f32 * 7.0 + 10.0).max(22.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::hover());
    ui.painter().rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.2, egui::Color32::from_rgb(145, 145, 145)),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        key,
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(200, 200, 200),
    );
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

        let viewport_rect = ui.available_rect_before_wrap();
        self.platform_sync_capture();
        let specs = [
            PanelSpec::new("logic_analyzer", "Logic Analyzer", 160.0),
            PanelSpec::new("node_graph", "Node Graph", 160.0),
        ];
        let mut panel_layout = std::mem::take(&mut self.panel_layout);
        let layout_response = panel_layout.show(
            ui,
            viewport_rect,
            STATUS_BAR_HEIGHT,
            &specs,
            |slot, panel_ui| match slot {
                PanelSlot::TitleBar("node_graph") => self.show_run_controls(panel_ui),
                PanelSlot::Body("logic_analyzer") => self.logic_analyzer.show(panel_ui),
                PanelSlot::Body("node_graph") => {
                    self.platform_before_graph();
                    self.node_graph.show(panel_ui);
                    if let Some(message) = self.node_graph.take_io_status() {
                        self.toasts.info(message);
                    }
                    self.platform_after_graph();
                }
                PanelSlot::TitleBar(_) | PanelSlot::Body(_) => {}
            },
        );
        self.panel_layout = panel_layout;

        let viewer = layout_response
            .panel("logic_analyzer")
            .expect("logic analyzer panel geometry");
        let graph = layout_response
            .panel("node_graph")
            .expect("node graph panel geometry");

        let pointer_pos = ui.input(|i| i.pointer.hover_pos());
        let modifiers = ui.input(|i| i.modifiers);
        let status_actions = self.status_actions(
            pointer_pos
                .is_some_and(|pos| !viewer.minimized && viewer.body_rect.contains(pos))
                .then(|| self.logic_analyzer.hovered_input_context()),
            pointer_pos.is_some_and(|pos| !graph.minimized && graph.body_rect.contains(pos)),
            modifiers,
        );
        let mut status_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("application-status-bar")
                .max_rect(layout_response.footer_rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
        );
        status_ui.set_clip_rect(layout_response.footer_rect);
        self.show_status_bar(&mut status_ui, &status_actions);

        self.about.show(ui.ctx());

        self.platform_after_ui(ui.ctx());

        self.toasts.show(ui.ctx());
    }
}

#[cfg(test)]
mod font_tests {
    use super::{install_fonts, load_symbol_fonts};

    #[test]
    fn application_input_bindings_are_valid() {
        input_bindings::InputBindings::from_json(include_str!("../config/input_bindings.json"))
            .expect("invalid application input binding configuration");
    }

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
