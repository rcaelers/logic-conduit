use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use input_bindings::{InputBindings, PointerButtonName, PointerGesture, Trigger};
use logic_analyzer_graph::{compiler, nodes};
use logic_analyzer_viewer::LogicAnalyzerViewer;
use node_graph::{GraphState, NodeBadge, NodeContextAction, NodeGraphWidget, NodeId};
use panel_layout::{BoundaryInteraction, PanelIcon, PanelLayout, PanelSlot, PanelSpec};

use crate::about::AboutWindow;
use crate::demo_signals;
use crate::live_capture::{
    CaptureAvailability, CaptureCoordinator, CaptureCoordinatorContract, capture_availability,
};
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

const SAMPLING_OVERLAY_EXTENSION: &str = "logic_analyzer_ui.sampling_overlay";

fn saved_sampling_overlay(graph: &GraphState) -> Result<Option<NodeId>, serde_json::Error> {
    graph.extension(SAMPLING_OVERLAY_EXTENSION)
}

fn save_sampling_overlay(
    graph: &mut GraphState,
    selected: Option<NodeId>,
) -> Result<(), serde_json::Error> {
    match selected {
        Some(selected) => graph.set_extension(SAMPLING_OVERLAY_EXTENSION, selected),
        None => {
            graph.remove_extension(SAMPLING_OVERLAY_EXTENSION);
            Ok(())
        }
    }
}

pub struct App {
    node_graph: NodeGraphWidget,
    logic_analyzer: LogicAnalyzerViewer,
    input_bindings: Arc<InputBindings>,
    panel_layout: PanelLayout,
    builders: compiler::BuilderRegistry,
    capture: CaptureCoordinator,
    capture_availability: CaptureAvailability,
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
    sampling_overlay_candidates: Vec<compiler::SamplingOverlayCandidate>,
    selected_sampling_overlay: Option<NodeId>,
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

    /// Builds the application around an initial graph supplied by the host
    /// application. The host owns where that graph comes from.
    pub fn new_with_graph(cc: &eframe::CreationContext, graph: node_graph::GraphState) -> Self {
        let mut app = Self::build(cc, |_ctx| {});
        app.node_graph.set_graph(graph);
        app.restore_sampling_overlay_setting();
        app
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
        let (platform, panel_layout_state, analyzer_split) =
            crate::app_platform::PlatformState::restore(cc, &mut widget);
        let mut logic_analyzer = LogicAnalyzerViewer::new();
        logic_analyzer.set_input_bindings(input_bindings.clone());
        let application_config = crate::application_config::load();
        logic_analyzer.set_color_profile(
            application_config
                .logic_analyzer_viewer
                .color_profile
                .into(),
        );
        logic_analyzer.set_channels(demo_signals::channels());
        Self {
            node_graph: widget,
            logic_analyzer,
            input_bindings,
            panel_layout: panel_layout_state.map_or_else(
                || {
                    PanelLayout::new([
                        ("logic_analyzer", analyzer_split),
                        ("node_graph", 1.0 - analyzer_split),
                    ])
                },
                PanelLayout::from_state,
            ),
            builders,
            capture: CaptureCoordinator::new(),
            capture_availability: CaptureAvailability::Unavailable {
                reason: "Checking the graph for a live capture source".into(),
            },
            run: None,
            run_message: None,
            toasts: Toasts::default(),
            platform,
            about: AboutWindow::new(),
            error_badges: Vec::new(),
            last_live_sync: -1.0,
            sampling_overlay_candidates: Vec::new(),
            selected_sampling_overlay: None,
        }
    }

    fn refresh_sampling_overlay_ui(&mut self) {
        let overlay = self.selected_sampling_overlay.and_then(|selected| {
            self.sampling_overlay_candidates
                .iter()
                .find(|candidate| candidate.node_id == selected)
                .map(|candidate| candidate.overlay.clone())
        });
        self.logic_analyzer.set_sampling_overlay(overlay);

        let mut actions: HashMap<NodeId, Vec<NodeContextAction>> = HashMap::new();
        for candidate in &self.sampling_overlay_candidates {
            let selected = self.selected_sampling_overlay == Some(candidate.node_id);
            let mut action = NodeContextAction::new("sampling_overlay", "Sampling Points")
                .with_checkmark(selected);
            if !selected {
                action = action.with_icon("◆");
            }
            actions.insert(candidate.node_id, vec![action]);
        }
        self.node_graph.set_node_context_actions(actions);
    }

    fn restore_sampling_overlay_setting(&mut self) {
        match saved_sampling_overlay(self.node_graph.graph()) {
            Ok(selected) => self.selected_sampling_overlay = selected,
            Err(error) => {
                self.selected_sampling_overlay = None;
                self.toasts.error(format!(
                    "Could not restore the graph's sampling-points setting: {error}"
                ));
            }
        }
        self.sampling_overlay_candidates.clear();
        self.refresh_sampling_overlay_ui();
    }

    fn persist_sampling_overlay_setting(&mut self) {
        let result =
            save_sampling_overlay(self.node_graph.graph_mut(), self.selected_sampling_overlay);
        if let Err(error) = result {
            self.toasts.error(format!(
                "Could not save the graph's sampling-points setting: {error}"
            ));
        }
    }

    fn set_sampling_overlay_candidates(
        &mut self,
        candidates: Vec<compiler::SamplingOverlayCandidate>,
    ) {
        self.sampling_overlay_candidates = candidates;
        if self.selected_sampling_overlay.is_some_and(|selected| {
            !self
                .sampling_overlay_candidates
                .iter()
                .any(|candidate| candidate.node_id == selected)
        }) {
            self.selected_sampling_overlay = None;
            self.persist_sampling_overlay_setting();
        }
        self.refresh_sampling_overlay_ui();
    }

    fn handle_node_context_action(&mut self, node_id: NodeId, action_id: &str) {
        if action_id != "sampling_overlay"
            || !self
                .sampling_overlay_candidates
                .iter()
                .any(|candidate| candidate.node_id == node_id)
        {
            return;
        }
        self.selected_sampling_overlay =
            (self.selected_sampling_overlay != Some(node_id)).then_some(node_id);
        self.persist_sampling_overlay_setting();
        self.refresh_sampling_overlay_ui();
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

    fn show_logic_analyzer_status(&mut self, ui: &mut egui::Ui) {
        self.show_capture_controls(ui);
        ui.separator();
        ui.label(egui::RichText::new(self.logic_analyzer.status_summary()).weak());
        if let Some(progress) = self.logic_analyzer.index_progress_fraction() {
            ui.add(
                egui::ProgressBar::new(progress)
                    .desired_width(64.0)
                    .show_percentage(),
            );
        }
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
                self.set_sampling_overlay_candidates(ctx.sampling_overlays);
                self.run = Some(run);
            }
            Err(errors) => {
                self.sampling_overlay_candidates.clear();
                self.refresh_sampling_overlay_ui();
                self.report_compile_errors(&errors);
            }
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
        if !self.is_running() && !self.capture.is_active() {
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

    fn start_capture_command(&mut self) {
        if self.capture.is_active() || self.is_running() {
            return;
        }
        for id in self.error_badges.drain(..) {
            self.node_graph.set_node_badge(id, None);
        }
        self.node_graph.clear_node_statuses();
        self.run_message = None;
        self.node_graph.sync_node_states();
        let compiled = match compiler::lower(self.node_graph.graph(), &self.builders) {
            Ok(compiled) => compiled,
            Err(errors) => {
                self.report_compile_errors(&errors);
                return;
            }
        };
        let feature = match compiler::discover_compiled_live_capture_feature(
            self.node_graph.graph(),
            &compiled,
            &self.builders,
        ) {
            Ok(Some(feature)) => feature,
            Ok(None) => {
                self.toasts.error("The graph has no live capture source");
                return;
            }
            Err(error) => {
                self.toasts.error(error.message);
                return;
            }
        };
        match self.capture.start(feature) {
            Ok(()) => self.node_graph.set_editing_enabled(false),
            Err(error) => self.toasts.error(error),
        }
    }

    fn stop_capture_command(&mut self) {
        self.capture.request_stop();
    }

    fn poll_capture(&mut self, ctx: &egui::Context) {
        self.capture.poll();
        self.node_graph
            .set_editing_enabled(self.capture.graph_editing_enabled());
        if self.capture.is_active() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn show_capture_controls(&mut self, ui: &mut egui::Ui) {
        let status = self.capture.status().cloned();
        if self.capture.is_active() {
            let state = status.as_ref().map(|status| status.state);
            if matches!(
                state,
                Some(
                    signal_processing::CaptureSessionState::Stopping
                        | signal_processing::CaptureSessionState::Error
                )
            ) {
                ui.add_enabled(false, egui::Button::new("⏹ Stop"));
                ui.spinner();
            } else if ui.small_button("⏹ Stop").clicked() {
                self.stop_capture_command();
            }
        } else {
            let availability = if self.is_running() {
                CaptureAvailability::Unavailable {
                    reason: "Stop the pipeline before starting capture".into(),
                }
            } else {
                self.capture_availability.clone()
            };
            let enabled = matches!(availability, CaptureAvailability::Available { .. });
            let response = ui.add_enabled(enabled, egui::Button::new("● Start"));
            match &availability {
                CaptureAvailability::Available {
                    source_node,
                    source_title,
                } => {
                    response.clone().on_hover_text(format!(
                        "Start capture from {source_title} (node {})",
                        source_node.0
                    ));
                }
                CaptureAvailability::Unavailable { .. } => {
                    if let Some(reason) = availability.reason() {
                        response.clone().on_disabled_hover_text(reason);
                    }
                }
            }
            if response.clicked() {
                self.start_capture_command();
                ui.ctx().request_repaint();
            }
        }

        if let Some(status) = self.capture.status() {
            let mut summary = capture_state_name(status.state).to_owned();
            if let Some(samples) = status.progress.captured_samples {
                summary.push_str(&format!(" · {samples} samples"));
            }
            if let Some(error) = &status.error {
                ui.colored_label(
                    egui::Color32::from_rgb(230, 120, 120),
                    format!("Error · {error}"),
                );
            } else {
                ui.label(summary);
            }
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
        let now = ctx.input(|input| input.time);
        if self.run.is_none() {
            if self.capture.is_active() {
                return;
            }
            if now - self.last_live_sync >= SYNC_INTERVAL_S {
                self.last_live_sync = now;
                self.capture_availability =
                    capture_availability(self.node_graph.graph(), &self.builders);
                if let Ok(candidates) =
                    compiler::sampling_overlay_candidates(self.node_graph.graph(), &self.builders)
                {
                    self.set_sampling_overlay_candidates(candidates);
                }
            }
            return;
        }
        let Some(run) = &mut self.run else {
            return;
        };
        run.pump(256);
        if !run.is_finished() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

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

        let mut refresh_sampling_overlays = false;
        match run.apply(self.node_graph.graph(), &self.builders) {
            Ok(summary) if summary.is_empty() => {
                refresh_sampling_overlays = true;
            }
            Ok(summary) => {
                refresh_sampling_overlays = true;
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
        if refresh_sampling_overlays {
            let candidates = run.sampling_overlays().to_vec();
            self.set_sampling_overlay_candidates(candidates);
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
                let about_button = ui.add_sized([52.0, 20.0], egui::Button::new("About"));
                if about_button.clicked() {
                    self.about.open();
                }
                ui.weak(self.node_graph.selection_summary());
                ui.weak(format!("{}%", self.node_graph.zoom_percent()));
            });
        });
    }

    fn show_run_controls(&mut self, ui: &mut egui::Ui) {
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
            let run = ui.add_enabled(
                !self.capture.is_active(),
                egui::Button::new("▶ Run").small(),
            );
            if self.capture.is_active() {
                run.clone()
                    .on_disabled_hover_text("Stop live capture before running the pipeline");
            }
            if run.clicked() {
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

    fn show_placeholder_panel(ui: &mut egui::Ui, title: &str) {
        ui.centered_and_justified(|ui| {
            ui.label(
                egui::RichText::new(format!("{title} panel"))
                    .size(16.0)
                    .weak(),
            );
        });
    }

    fn show_view_panel(&mut self, content_id: &str) {
        self.panel_layout.ensure_right_column_content(
            content_id,
            &VIEW_PANEL_ORDER,
            RIGHT_COLUMN_LAYOUT_FRACTION,
        );
    }

    fn reset_panel_layout(&mut self) {
        self.panel_layout = PanelLayout::new([
            ("logic_analyzer", DEFAULT_ANALYZER_SPLIT),
            ("node_graph", 1.0 - DEFAULT_ANALYZER_SPLIT),
        ]);
    }

    fn status_actions(
        &self,
        boundary_interaction: Option<BoundaryInteraction>,
        over_panel_title: bool,
        viewer_context: Option<&str>,
        over_graph: bool,
        modifiers: egui::Modifiers,
    ) -> Vec<StatusAction> {
        let contexts = if boundary_interaction == Some(BoundaryInteraction::Dragging) {
            vec!["panel_boundary.dragging", "global"]
        } else if let Some(graph_context) = self.node_graph.active_input_context() {
            vec![graph_context, "global"]
        } else if boundary_interaction == Some(BoundaryInteraction::Hovered) {
            vec!["panel_boundary", "global"]
        } else if over_panel_title {
            vec!["panel_title", "global"]
        } else if over_graph {
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
const DEFAULT_ANALYZER_SPLIT: f32 = 0.42;
const RIGHT_COLUMN_LAYOUT_FRACTION: f32 = 0.75;
const VIEW_PANEL_ORDER: [&str; 3] = ["watches", "triggers", "decoder"];

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

fn capture_state_name(state: signal_processing::CaptureSessionState) -> &'static str {
    match state {
        signal_processing::CaptureSessionState::Preparing => "Preparing",
        signal_processing::CaptureSessionState::Prepared => "Prepared",
        signal_processing::CaptureSessionState::Armed => "Armed",
        signal_processing::CaptureSessionState::Triggered => "Triggered",
        signal_processing::CaptureSessionState::Recording => "Recording",
        signal_processing::CaptureSessionState::Stopping => "Stopping…",
        signal_processing::CaptureSessionState::Complete => "Complete",
        signal_processing::CaptureSessionState::Error => "Error",
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
        self.node_graph.filter_modal_raw_input(raw_input);
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
        self.poll_capture(ui.ctx());
        self.platform_sync_capture();
        self.sync_run(ui.ctx());
        let specs = [
            PanelSpec::new("logic_analyzer", "Logic Analyzer", 160.0)
                .icon(PanelIcon::Waveform)
                .minimum_width(220.0)
                .singleton(),
            PanelSpec::new("node_graph", "Node Graph", 160.0)
                .icon(PanelIcon::Network)
                .minimum_width(220.0)
                .singleton(),
            PanelSpec::new("watches", "Watches", 120.0)
                .icon(PanelIcon::List)
                .minimum_width(180.0),
            PanelSpec::new("triggers", "Triggers", 120.0)
                .icon(PanelIcon::Target)
                .minimum_width(180.0),
            PanelSpec::new("decoder", "Decoder", 120.0)
                .icon(PanelIcon::Table)
                .minimum_width(220.0),
        ];
        let mut panel_layout = std::mem::take(&mut self.panel_layout);
        panel_layout
            .set_maximize_shortcut(self.input_bindings.shortcut(&["panel"], "toggle_maximize"));
        let layout_response = panel_layout.show(
            ui,
            viewport_rect,
            STATUS_BAR_HEIGHT,
            &specs,
            |slot, panel_ui| match slot {
                PanelSlot::TitleBar {
                    content_id: "logic_analyzer",
                    ..
                } => {
                    self.show_logic_analyzer_status(panel_ui);
                }
                PanelSlot::TitleBar {
                    content_id: "node_graph",
                    ..
                } => self.show_run_controls(panel_ui),
                PanelSlot::Body {
                    content_id: "logic_analyzer",
                    ..
                } => self.logic_analyzer.show(panel_ui),
                PanelSlot::Body {
                    content_id: "node_graph",
                    ..
                } => {
                    self.platform_before_graph();
                    self.node_graph.show(panel_ui);
                    if let Some(message) = self.node_graph.take_io_status() {
                        self.toasts.info(message);
                    }
                    if let Some((node_id, action_id)) = self.node_graph.take_node_context_action() {
                        self.handle_node_context_action(node_id, &action_id);
                    }
                    self.platform_after_graph();
                }
                PanelSlot::Body {
                    content_id: "watches",
                    ..
                } => Self::show_placeholder_panel(panel_ui, "Watches"),
                PanelSlot::Body {
                    content_id: "triggers",
                    ..
                } => Self::show_placeholder_panel(panel_ui, "Triggers"),
                PanelSlot::Body {
                    content_id: "decoder",
                    ..
                } => Self::show_placeholder_panel(panel_ui, "Decoder"),
                PanelSlot::TitleBar { .. } | PanelSlot::Body { .. } => {}
            },
        );
        self.panel_layout = panel_layout;

        let viewer = layout_response.content_panel("logic_analyzer");
        let graph = layout_response.content_panel("node_graph");

        let pointer_pos = ui.input(|i| i.pointer.hover_pos());
        let modifiers = ui.input(|i| i.modifiers);
        let over_panel_title = pointer_pos.is_some_and(|pos| {
            layout_response
                .panels
                .iter()
                .any(|panel| panel.title_rect.contains(pos))
        });
        let status_actions = self.status_actions(
            layout_response.boundary_interaction,
            over_panel_title,
            viewer
                .filter(|viewer| pointer_pos.is_some_and(|pos| viewer.body_rect.contains(pos)))
                .map(|_| self.logic_analyzer.hovered_input_context()),
            graph.is_some_and(|graph| pointer_pos.is_some_and(|pos| graph.body_rect.contains(pos))),
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
    use node_graph::{GraphState, NodeId};

    use super::{install_fonts, load_symbol_fonts, save_sampling_overlay, saved_sampling_overlay};

    #[test]
    fn application_input_bindings_are_valid() {
        let bindings =
            input_bindings::InputBindings::from_json(include_str!("../config/input_bindings.json"))
                .expect("invalid application input binding configuration");
        assert_eq!(
            bindings.shortcut(&["panel"], "toggle_maximize"),
            Some(egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL,
                egui::Key::Space,
            ))
        );
    }

    #[test]
    fn sampling_overlay_selection_round_trips_with_the_graph_document() {
        let mut graph = GraphState::default();
        save_sampling_overlay(&mut graph, Some(NodeId(17))).unwrap();

        let json = serde_json::to_string(&graph).unwrap();
        let mut restored: GraphState = serde_json::from_str(&json).unwrap();
        assert_eq!(saved_sampling_overlay(&restored).unwrap(), Some(NodeId(17)));

        save_sampling_overlay(&mut restored, None).unwrap();
        assert_eq!(saved_sampling_overlay(&restored).unwrap(), None);
    }

    #[test]
    fn interaction_status_bindings_change_during_panel_and_node_drags() {
        let bindings =
            input_bindings::InputBindings::from_json(include_str!("../config/input_bindings.json"))
                .expect("invalid application input binding configuration");

        let boundary: Vec<_> = bindings
            .status_bindings(&["panel_boundary"], egui::Modifiers::NONE)
            .into_iter()
            .map(|binding| binding.label.as_str())
            .collect();
        assert_eq!(boundary, ["Resize Panels", "Panel Options"]);

        let resizing: Vec<_> = bindings
            .status_bindings(&["panel_boundary.dragging"], egui::Modifiers::NONE)
            .into_iter()
            .map(|binding| binding.label.as_str())
            .collect();
        assert_eq!(resizing, ["Finish Resize"]);

        let title_bar: Vec<_> = bindings
            .status_bindings(&["panel_title"], egui::Modifiers::NONE)
            .into_iter()
            .map(|binding| binding.label.as_str())
            .collect();
        assert_eq!(title_bar, ["Maximize / Restore Area", "Area Options"]);

        let dragging: Vec<_> = bindings
            .status_bindings(&["node_graph.drag_node"], egui::Modifiers::NONE)
            .into_iter()
            .map(|binding| binding.label.as_str())
            .collect();
        assert_eq!(dragging, ["Confirm", "Cancel", "X Axis", "Y Axis"]);

        let snapping: Vec<_> = bindings
            .status_bindings(&["node_graph.drag_node"], egui::Modifiers::CTRL)
            .into_iter()
            .map(|binding| binding.label.as_str())
            .collect();
        assert_eq!(
            snapping,
            ["Confirm", "Cancel", "Snap to Grid", "X Axis", "Y Axis"]
        );

        let wire_drag: Vec<_> = bindings
            .status_bindings(&["node_graph.drag_wire"], egui::Modifiers::NONE)
            .into_iter()
            .map(|binding| binding.label.as_str())
            .collect();
        assert_eq!(wire_drag, ["Drag Node-link", "Confirm Link", "Cancel"]);
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
