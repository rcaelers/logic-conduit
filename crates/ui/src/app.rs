use crate::about::AboutWindow;
use crate::compiler;
use crate::demo_signals;
use crate::nodes;
use crate::toast::Toasts;
use logic_analyzer_viewer::LogicAnalyzerViewer;
use node_graph::{NodeBadge, NodeGraphWidget, NodeId};
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

#[cfg(not(target_arch = "wasm32"))]
enum FileCommand {
    New,
    Load,
    LoadPath(PathBuf),
    ClearRecent,
    Save,
    SaveAs,
    Quit,
}

/// A destructive action (quit, new, or loading over the current graph) that
/// must not proceed silently while there are unsaved changes. Set when the
/// action is requested over a dirty graph; `show_guarded_action_dialog`
/// resolves it (Save/Don't Save/Cancel) and either runs the action or drops
/// it (Phase 5.1).
#[cfg(not(target_arch = "wasm32"))]
enum GuardedAction {
    Quit,
    New,
    LoadPath(PathBuf),
}

/// UI/session state persisted across launches via `eframe::Storage`
/// (Phase 5.2) — the graph document itself is never part of this; only
/// layout and recently-opened files.
#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedState {
    analyzer_split: f32,
    graph_ui_prefs: node_graph::GraphUiPrefs,
    recent_files: Vec<PathBuf>,
}

#[cfg(not(target_arch = "wasm32"))]
const MAX_RECENT_FILES: usize = 10;

/// Canonicalizes every path and drops later duplicates, keeping first
/// occurrence (i.e. most-recent-first order survives) and capping at
/// `MAX_RECENT_FILES`. Applied both when loading a persisted list (in case
/// it was saved by a build before this normalization existed, or before a
/// canonicalization edge case was fixed — old duplicate entries would
/// otherwise never clear themselves out) and after every push, so the
/// stored list is always self-healing rather than only preventing *new*
/// duplicates.
#[cfg(not(target_arch = "wasm32"))]
fn normalize_recent_files(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for path in paths {
        let canonical = path.canonicalize().unwrap_or(path);
        if seen.insert(canonical.clone()) {
            result.push(canonical);
        }
        if result.len() >= MAX_RECENT_FILES {
            break;
        }
    }
    result
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "macos")))]
fn is_quit_shortcut(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key: egui::Key::Q,
            pressed: true,
            modifiers,
            ..
        } if modifiers.matches_logically(egui::Modifiers::COMMAND)
    )
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
pub enum NativeMenuCommand {
    About,
    New,
    Load,
    LoadPath(std::path::PathBuf),
    ClearRecent,
    Save,
    SaveAs,
    Quit,
    Run,
    Stop,
}

#[cfg(target_os = "macos")]
struct NativeMenuBridge {
    sender: crossbeam_channel::Sender<NativeMenuCommand>,
    context: egui::Context,
}

#[cfg(target_os = "macos")]
static NATIVE_MENU_BRIDGE: std::sync::OnceLock<NativeMenuBridge> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
pub fn dispatch_native_menu_command(command: NativeMenuCommand) {
    if let Some(bridge) = NATIVE_MENU_BRIDGE.get() {
        let _ = bridge.sender.send(command);
        bridge.context.request_repaint();
    }
}

/// Reverse direction of `NATIVE_MENU_BRIDGE`: app state → native menu. The
/// only current use is keeping the native "Open Recent" submenu live as
/// files are opened/saved during the session, instead of only reflecting
/// what was persisted as of the last launch. `main.rs` registers this once,
/// on startup, with a closure that rebuilds the Cocoa submenu; both the
/// registration and every call happen on the main thread (menu mutation
/// requires it), so no synchronization beyond `OnceLock`'s own is needed.
#[cfg(target_os = "macos")]
static RECENT_FILES_LISTENER: std::sync::OnceLock<Box<dyn Fn(&[PathBuf]) + Send + Sync>> =
    std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
pub fn set_recent_files_listener(listener: impl Fn(&[PathBuf]) + Send + Sync + 'static) {
    let _ = RECENT_FILES_LISTENER.set(Box::new(listener));
}

#[cfg(target_os = "macos")]
fn notify_recent_files_changed(paths: &[PathBuf]) {
    if let Some(listener) = RECENT_FILES_LISTENER.get() {
        listener(paths);
    }
}

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
    #[cfg(not(target_arch = "wasm32"))]
    current_file: Option<PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    saved_graph: serde_json::Value,
    #[cfg(not(target_arch = "wasm32"))]
    pending_guarded_action: Option<GuardedAction>,
    #[cfg(not(target_arch = "wasm32"))]
    allow_close: bool,
    /// MRU list, most recent first, max `MAX_RECENT_FILES`, deduped.
    #[cfg(not(target_arch = "wasm32"))]
    recent_files: Vec<PathBuf>,
    /// Set while the "Clear the recent files list?" confirmation is up.
    #[cfg(not(target_arch = "wasm32"))]
    confirm_clear_recent: bool,
    #[cfg(target_os = "macos")]
    native_menu_commands: crossbeam_channel::Receiver<NativeMenuCommand>,
    about: AboutWindow,
    /// Nodes badged with compile errors; cleared on the next Run.
    error_badges: Vec<NodeId>,
    /// Last time the running pipeline was diffed against the edited graph.
    last_live_sync: f64,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        Self::new_with_plugins(cc, |_ctx| {})
    }

    /// The persisted MRU list, most recent first — read once at startup by
    /// the native macOS menu to build its "Open Recent" submenu (Phase 5.1).
    /// Empty on wasm, where there is no recent-files list at all.
    pub fn recent_files(&self) -> &[std::path::PathBuf] {
        #[cfg(not(target_arch = "wasm32"))]
        {
            &self.recent_files
        }
        #[cfg(target_arch = "wasm32")]
        {
            &[]
        }
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

        #[cfg(not(target_arch = "wasm32"))]
        let persisted = cc
            .storage
            .and_then(|storage| eframe::get_value::<PersistedState>(storage, eframe::APP_KEY));
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(persisted) = &persisted {
            widget.set_ui_prefs(persisted.graph_ui_prefs.clone());
        }
        #[cfg(not(target_arch = "wasm32"))]
        let analyzer_split = persisted.as_ref().map_or(0.42, |p| p.analyzer_split);
        #[cfg(target_arch = "wasm32")]
        let analyzer_split = 0.42;
        #[cfg(not(target_arch = "wasm32"))]
        let recent_files = persisted
            .map(|p| normalize_recent_files(p.recent_files))
            .unwrap_or_default();

        #[cfg(not(target_arch = "wasm32"))]
        let saved_graph = widget
            .snapshot_value()
            .expect("new graph should always serialize");
        let mut logic_analyzer = LogicAnalyzerViewer::new();
        logic_analyzer.set_channels(demo_signals::channels());
        #[cfg(target_os = "macos")]
        let native_menu_commands = {
            let (sender, receiver) = crossbeam_channel::unbounded();
            assert!(
                NATIVE_MENU_BRIDGE
                    .set(NativeMenuBridge {
                        sender,
                        context: cc.egui_ctx.clone(),
                    })
                    .is_ok(),
                "only one native application instance is supported"
            );
            receiver
        };
        Self {
            node_graph: widget,
            logic_analyzer,
            analyzer_split,
            builders,
            run: None,
            run_message: None,
            toasts: Toasts::default(),
            #[cfg(not(target_arch = "wasm32"))]
            current_file: None,
            #[cfg(not(target_arch = "wasm32"))]
            saved_graph,
            #[cfg(not(target_arch = "wasm32"))]
            pending_guarded_action: None,
            #[cfg(not(target_arch = "wasm32"))]
            allow_close: false,
            #[cfg(not(target_arch = "wasm32"))]
            recent_files,
            #[cfg(not(target_arch = "wasm32"))]
            confirm_clear_recent: false,
            #[cfg(target_os = "macos")]
            native_menu_commands,
            about: AboutWindow::new(),
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
                self.mark_graph_saved();
                self.push_recent_file(path.clone());
                self.toasts.info(format!("Loaded {}", path.display()));
            }
            Err(error) => self.toasts.error(error),
        }
    }

    /// Inserts `path` at the front of the MRU list, deduping and capping at
    /// `MAX_RECENT_FILES` (Phase 5.1).
    #[cfg(not(target_arch = "wasm32"))]
    fn push_recent_file(&mut self, path: PathBuf) {
        let mut paths = self.recent_files.clone();
        paths.insert(0, path);
        self.recent_files = normalize_recent_files(paths);
        #[cfg(target_os = "macos")]
        notify_recent_files_changed(&self.recent_files);
    }

    /// Resets to a fresh, empty graph — File → New (Phase 5.1). Assumes the
    /// unsaved-changes guard has already been resolved by the caller.
    #[cfg(not(target_arch = "wasm32"))]
    fn do_new(&mut self) {
        if let Some(run) = &mut self.run {
            run.stop();
        }
        self.run_message = None;
        self.error_badges.clear();
        self.node_graph.new_graph();
        self.current_file = None;
        self.mark_graph_saved();
        self.toasts.info("New graph");
    }

    /// Requests File → New, guarding on unsaved changes the same way
    /// `request_quit` does.
    #[cfg(not(target_arch = "wasm32"))]
    fn request_new(&mut self) {
        if self.has_unsaved_changes() {
            self.pending_guarded_action = Some(GuardedAction::New);
        } else {
            self.do_new();
        }
    }

    /// Requests loading `path` (e.g. from Open Recent), guarding on unsaved
    /// changes the same way `request_quit` does.
    #[cfg(not(target_arch = "wasm32"))]
    fn request_load_path(&mut self, path: PathBuf) {
        if self.has_unsaved_changes() {
            self.pending_guarded_action = Some(GuardedAction::LoadPath(path));
        } else {
            self.load_file(path);
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
    fn save_file(&mut self) -> bool {
        let Some(path) = self.current_file.clone() else {
            return self.save_file_as();
        };
        self.save_to_file(path)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_file_as(&mut self) -> bool {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save graph as")
            .set_file_name("pipeline.json")
            .add_filter("Graph JSON", &["json"]);
        if let Some(path) = &self.current_file {
            if let Some(parent) = path.parent() {
                dialog = dialog.set_directory(parent);
            }
            if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
                dialog = dialog.set_file_name(file_name);
            }
        }
        let path = dialog.save_file();
        let Some(path) = path else {
            return false;
        };
        self.save_to_file(path)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_to_file(&mut self, path: PathBuf) -> bool {
        match self.node_graph.save_to_path(&path) {
            Ok(()) => {
                self.current_file = Some(path.clone());
                self.mark_graph_saved();
                self.push_recent_file(path.clone());
                self.toasts.info(format!("Saved {}", path.display()));
                true
            }
            Err(error) => {
                self.toasts.error(error);
                false
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn mark_graph_saved(&mut self) {
        match self.node_graph.snapshot_value() {
            Ok(graph) => self.saved_graph = graph,
            Err(error) => self.toasts.error(error),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn has_unsaved_changes(&mut self) -> bool {
        self.node_graph
            .snapshot_value()
            .map_or(true, |graph| graph != self.saved_graph)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn request_quit(&mut self, ctx: &egui::Context) {
        if self.has_unsaved_changes() {
            self.pending_guarded_action = Some(GuardedAction::Quit);
        } else {
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn execute_file_command(&mut self, command: FileCommand, ctx: &egui::Context) {
        match command {
            FileCommand::New => self.request_new(),
            FileCommand::Load => self.choose_and_load_file(),
            FileCommand::LoadPath(path) => self.request_load_path(path),
            FileCommand::ClearRecent => self.confirm_clear_recent = true,
            FileCommand::Save => {
                self.save_file();
            }
            FileCommand::SaveAs => {
                self.save_file_as();
            }
            FileCommand::Quit => self.request_quit(ctx),
        }
    }

    /// Resolves whatever `pending_guarded_action` (quit/new/load-over-dirty)
    /// is outstanding — Save/Don't Save/Cancel, same dialog for all three
    /// (Phase 5.1).
    #[cfg(not(target_arch = "wasm32"))]
    fn show_guarded_action_dialog(&mut self, ctx: &egui::Context) {
        if self.pending_guarded_action.is_none() {
            return;
        }

        enum DialogChoice {
            Save,
            Discard,
            Cancel,
        }

        let mut choice = None;
        egui::Window::new("Save changes?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Save changes to the graph before continuing?");
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        choice = Some(DialogChoice::Save);
                    }
                    if ui.button("Don't Save").clicked() {
                        choice = Some(DialogChoice::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(DialogChoice::Cancel);
                    }
                });
            });

        match choice {
            // Save can itself open a blocking Save As dialog and be
            // cancelled — leave `pending_guarded_action` set so this dialog
            // simply reopens next frame rather than silently dropping the
            // action.
            Some(DialogChoice::Save) if self.save_file() => self.complete_guarded_action(ctx),
            Some(DialogChoice::Discard) => self.complete_guarded_action(ctx),
            Some(DialogChoice::Cancel) => self.pending_guarded_action = None,
            _ => {}
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn complete_guarded_action(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending_guarded_action.take() else {
            return;
        };
        match action {
            GuardedAction::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            GuardedAction::New => self.do_new(),
            GuardedAction::LoadPath(path) => self.load_file(path),
        }
    }

    /// Resolves the "Clear the recent files list?" confirmation triggered
    /// by either the egui or native "Clear Recent" menu item.
    #[cfg(not(target_arch = "wasm32"))]
    fn show_clear_recent_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_clear_recent {
            return;
        }

        enum DialogChoice {
            Clear,
            Cancel,
        }

        let mut choice = None;
        egui::Window::new("Clear recent files?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Remove all entries from the recent files list?");
                ui.horizontal(|ui| {
                    if ui.button("Clear").clicked() {
                        choice = Some(DialogChoice::Clear);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(DialogChoice::Cancel);
                    }
                });
            });

        match choice {
            Some(DialogChoice::Clear) => {
                self.recent_files.clear();
                #[cfg(target_os = "macos")]
                notify_recent_files_changed(&[]);
                self.confirm_clear_recent = false;
            }
            Some(DialogChoice::Cancel) => self.confirm_clear_recent = false,
            None => {}
        }
    }

    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "macos")))]
    fn show_menu_bar(&mut self, ui: &mut egui::Ui) {
        let new_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::N);
        let load_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O);
        let save_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S);
        let save_as_shortcut = egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::S,
        );
        let quit_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Q);
        let run_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::R);
        let stop_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Period);
        let mut command = if ui.input_mut(|input| input.consume_shortcut(&new_shortcut)) {
            Some(FileCommand::New)
        } else if ui.input_mut(|input| input.consume_shortcut(&load_shortcut)) {
            Some(FileCommand::Load)
        } else if ui.input_mut(|input| input.consume_shortcut(&save_as_shortcut)) {
            Some(FileCommand::SaveAs)
        } else if ui.input_mut(|input| input.consume_shortcut(&save_shortcut)) {
            Some(FileCommand::Save)
        } else if ui.input_mut(|input| input.consume_shortcut(&quit_shortcut)) {
            Some(FileCommand::Quit)
        } else {
            None
        };
        // Not routed through `command`/`execute_file_command` like the File
        // items above — Run/Stop are self-contained and idempotent
        // (`run_command`/`stop_command` no-op when they don't apply), so
        // there's nothing to defer.
        if ui.input_mut(|input| input.consume_shortcut(&run_shortcut)) {
            self.run_command();
        } else if ui.input_mut(|input| input.consume_shortcut(&stop_shortcut)) {
            self.stop_command();
        }

        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add(
                        egui::Button::new("New")
                            .shortcut_text(ui.ctx().format_shortcut(&new_shortcut)),
                    )
                    .clicked()
                {
                    command = Some(FileCommand::New);
                    ui.close();
                }
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
                ui.menu_button("Open Recent", |ui| {
                    let existing: Vec<PathBuf> = self
                        .recent_files
                        .iter()
                        .filter(|path| path.exists())
                        .cloned()
                        .collect();
                    if existing.is_empty() {
                        ui.weak("No recent files");
                    } else {
                        for path in &existing {
                            let label = path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("?");
                            if ui.button(label).clicked() {
                                command = Some(FileCommand::LoadPath(path.clone()));
                                ui.close();
                            }
                        }
                    }
                    ui.separator();
                    if ui
                        .add_enabled(!existing.is_empty(), egui::Button::new("Clear Recent"))
                        .clicked()
                    {
                        command = Some(FileCommand::ClearRecent);
                        ui.close();
                    }
                });
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
                if ui
                    .add(
                        egui::Button::new("Save As...")
                            .shortcut_text(ui.ctx().format_shortcut(&save_as_shortcut)),
                    )
                    .clicked()
                {
                    command = Some(FileCommand::SaveAs);
                    ui.close();
                }
                ui.separator();
                if ui
                    .add(
                        egui::Button::new("Quit")
                            .shortcut_text(ui.ctx().format_shortcut(&quit_shortcut)),
                    )
                    .clicked()
                {
                    command = Some(FileCommand::Quit);
                    ui.close();
                }
            });
            ui.menu_button("Pipeline", |ui| {
                if ui
                    .add(
                        egui::Button::new("Run")
                            .shortcut_text(ui.ctx().format_shortcut(&run_shortcut)),
                    )
                    .clicked()
                {
                    self.run_command();
                    ui.close();
                }
                if ui
                    .add(
                        egui::Button::new("Stop")
                            .shortcut_text(ui.ctx().format_shortcut(&stop_shortcut)),
                    )
                    .clicked()
                {
                    self.stop_command();
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About DSL Pipeline Editor").clicked() {
                    self.about.open();
                    ui.close();
                }
            });
        });

        if let Some(command) = command {
            self.execute_file_command(command, ui.ctx());
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
        self.logic_analyzer
            .set_derived_lanes(ctx.derived_lanes.clone());

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
        if self.is_running() && !self.is_stopping()
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

#[cfg(all(test, not(target_arch = "wasm32"), not(target_os = "macos")))]
mod close_shortcut_tests {
    use super::is_quit_shortcut;

    #[test]
    fn recognizes_command_q_key_presses() {
        let press = egui::Event::Key {
            key: egui::Key::Q,
            physical_key: Some(egui::Key::Q),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        };
        let release = egui::Event::Key {
            key: egui::Key::Q,
            physical_key: Some(egui::Key::Q),
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        };

        assert!(is_quit_shortcut(&press));
        assert!(!is_quit_shortcut(&release));
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, _raw_input: &mut egui::RawInput) {
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "macos")))]
        {
            let quit_requested = _raw_input.events.iter().any(is_quit_shortcut);
            _raw_input.events.retain(|event| !is_quit_shortcut(event));
            if quit_requested && !self.allow_close {
                self.request_quit(_ctx);
            }
        }
    }

    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(target_os = "macos")]
        while let Ok(command) = self.native_menu_commands.try_recv() {
            let command = match command {
                NativeMenuCommand::About => {
                    self.about.open();
                    continue;
                }
                NativeMenuCommand::Run => {
                    self.run_command();
                    continue;
                }
                NativeMenuCommand::Stop => {
                    self.stop_command();
                    continue;
                }
                NativeMenuCommand::New => FileCommand::New,
                NativeMenuCommand::Load => FileCommand::Load,
                NativeMenuCommand::LoadPath(path) => FileCommand::LoadPath(path),
                NativeMenuCommand::ClearRecent => FileCommand::ClearRecent,
                NativeMenuCommand::Save => FileCommand::Save,
                NativeMenuCommand::SaveAs => FileCommand::SaveAs,
                NativeMenuCommand::Quit => FileCommand::Quit,
            };
            self.execute_file_command(command, _ctx);
        }

        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "macos")))]
        {
            let os_close_requested = _ctx.input(|input| input.viewport().close_requested());
            if !self.allow_close && os_close_requested {
                if self.has_unsaved_changes() {
                    self.pending_guarded_action = Some(GuardedAction::Quit);
                    _ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                } else {
                    self.allow_close = true;
                    _ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let state = PersistedState {
            analyzer_split: self.analyzer_split,
            graph_ui_prefs: self.node_graph.ui_prefs(),
            recent_files: self.recent_files.clone(),
        };
        eframe::set_value(storage, eframe::APP_KEY, &state);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "macos")))]
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

        ui.allocate_ui(egui::vec2(available.x, graph_height), |ui| {
            self.node_graph.show(ui);
        });
        if let Some(message) = self.node_graph.take_io_status() {
            self.toasts.info(message);
        }

        self.about.show(ui.ctx());

        #[cfg(not(target_arch = "wasm32"))]
        self.show_guarded_action_dialog(ui.ctx());
        #[cfg(not(target_arch = "wasm32"))]
        self.show_clear_recent_dialog(ui.ctx());

        self.toasts.show(ui.ctx());
    }
}
