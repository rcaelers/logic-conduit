use std::path::PathBuf;

use logic_analyzer_graph as compiler;
use node_graph::NodeId;

use crate::app::App;
use crate::app_platform::{FileCommand, GuardedAction, derived_cache_directory};
#[cfg(target_os = "macos")]
use crate::app_platform::{NativeMenuCommand, notify_recent_files_changed};
use crate::live_capture::{CaptureCoordinatorContract, CaptureRawExportFormat};
#[cfg(not(target_os = "macos"))]
use crate::product::APPLICATION_NAME;

impl App {
    pub(crate) fn platform_clear_capture_caches(
        &mut self,
        configs: &[signal_processing::PersistentStoreConfig],
    ) -> Result<(), String> {
        for config in configs {
            signal_processing::clear_cache_entry(config).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn can_replace_graph(&mut self) -> bool {
        if self.capture.is_active() || self.is_capture_analysis_active() {
            self.toasts
                .error("Wait for live capture analysis before replacing the graph");
            false
        } else {
            true
        }
    }

    pub(crate) fn platform_load_startup_file(&mut self, file: Option<&std::path::Path>) {
        if let Some(file) = file {
            self.load_file(file.to_owned());
        }
    }

    pub(crate) fn platform_prepare_run(&mut self, ctx: &mut compiler::CompileCtx) {
        self.refresh_derived_cache_nodes();
        ctx.set_persistent_cache_directory(derived_cache_directory());
    }

    pub(crate) fn platform_raw_input_hook(
        &mut self,
        _ctx: &egui::Context,
        _raw_input: &mut egui::RawInput,
    ) {
    }

    pub(crate) fn platform_logic(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "macos")]
        while let Ok(command) = self.platform.native_menu_commands.try_recv() {
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
                NativeMenuCommand::ClearDerivedCaches => {
                    self.request_clear_all_derived_caches();
                    continue;
                }
                NativeMenuCommand::ShowWatches => {
                    self.show_view_panel("watches");
                    continue;
                }
                NativeMenuCommand::ShowTriggers => {
                    self.show_view_panel("triggers");
                    continue;
                }
                NativeMenuCommand::ShowDecoder => {
                    self.show_view_panel("decoder");
                    continue;
                }
                NativeMenuCommand::ResetLayout => {
                    self.reset_panel_layout();
                    continue;
                }
                NativeMenuCommand::New => FileCommand::New,
                NativeMenuCommand::Load => FileCommand::Load,
                NativeMenuCommand::LoadPath(path) => FileCommand::LoadPath(path),
                NativeMenuCommand::ClearRecent => FileCommand::ClearRecent,
                NativeMenuCommand::Save => FileCommand::Save,
                NativeMenuCommand::SaveAs => FileCommand::SaveAs,
                NativeMenuCommand::SaveCaptureData => FileCommand::SaveCaptureData,
                NativeMenuCommand::Quit => FileCommand::Quit,
            };
            self.execute_file_command(command, ctx);
        }

        #[cfg(not(target_os = "macos"))]
        {
            let close_requested = ctx.input(|input| input.viewport().close_requested());
            if !self.platform.allow_close && close_requested {
                if self.has_unsaved_changes() {
                    self.platform.pending_guarded_action = Some(GuardedAction::Quit);
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                } else {
                    self.platform.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    pub(crate) fn platform_save(&mut self, storage: &mut dyn eframe::Storage) {
        self.platform.save(storage, self.node_graph.ui_prefs());
    }

    pub(crate) fn platform_before_ui(&mut self, _ui: &mut egui::Ui) {
        #[cfg(not(target_os = "macos"))]
        self.show_menu_bar(_ui);
    }

    pub(crate) fn platform_sync_capture(&mut self) {
        if self.logic_analyzer.has_growing_capture() {
            return;
        }
        let discovered =
            compiler::discover_capture_presentation(self.node_graph.graph(), &self.builders)
                .ok()
                .flatten();
        let Some(discovered) = discovered else {
            if self.platform.capture_presentation_identity.take().is_some() {
                self.logic_analyzer.clear_capture();
            }
            return;
        };
        if self.platform.capture_presentation_identity.as_deref()
            == Some(discovered.identity.as_str())
        {
            return;
        }
        self.platform.capture_presentation_identity = Some(discovered.identity);
        match discovered.presentation {
            compiler::CapturePresentation::Indexed {
                identity, factory, ..
            } => {
                self.logic_analyzer.set_capture_factory(identity, factory);
            }
            compiler::CapturePresentation::InMemory { signals, .. } => {
                self.set_capture_preview(signals);
            }
            compiler::CapturePresentation::Channels(channels) => {
                self.logic_analyzer.set_channels(
                    channels
                        .into_iter()
                        .map(|(index, name)| logic_analyzer_viewer::ChannelSignal {
                            index,
                            name,
                            initial: false,
                            transitions: Vec::new(),
                        })
                        .collect(),
                );
            }
        }
    }

    pub(crate) fn platform_restore_graph_capture(&mut self) {
        self.platform.capture_presentation_identity = None;
    }

    pub(crate) fn platform_before_graph(&mut self) {
        self.node_graph
            .set_derived_cache_nodes(self.platform.derived_cache_nodes.iter().copied());
    }

    pub(crate) fn platform_after_graph(&mut self) {
        if let Some(node_id) = self.node_graph.take_clear_derived_cache_request() {
            self.clear_node_derived_cache(node_id);
        }
    }

    pub(crate) fn platform_after_ui(&mut self, ctx: &egui::Context) {
        self.show_guarded_action_dialog(ctx);
        self.show_clear_recent_dialog(ctx);
        self.show_clear_derived_caches_dialog(ctx);
    }
    fn load_file(&mut self, path: PathBuf) {
        if !self.can_replace_graph() {
            return;
        }
        match self.node_graph.load_from_path(&path) {
            Ok(()) => {
                if let Some(run) = &mut self.run {
                    run.stop();
                }
                self.capture.clear_completed();
                self.run_message = None;
                self.error_badges.clear();
                self.synchronize_payload_subscription_manifest(true);
                self.restore_sampling_overlay_setting();
                self.restore_viewer_lane_order_setting();
                self.restore_panel_layout_setting();
                self.platform.current_file = Some(path.clone());
                self.mark_graph_saved();
                self.push_recent_file(path.clone());
                self.refresh_derived_cache_nodes();
                self.toasts.info(format!("Loaded {}", path.display()));
            }
            Err(error) => self.toasts.error(error),
        }
    }

    /// Inserts `path` at the front of the MRU list, deduping and capping at
    /// `MAX_RECENT_FILES` (Phase 5.1).
    fn push_recent_file(&mut self, path: PathBuf) {
        self.platform.push_recent_file(path);
        #[cfg(target_os = "macos")]
        notify_recent_files_changed(&self.platform.recent_files);
    }

    /// Resets to a fresh, empty graph — File → New (Phase 5.1). Assumes the
    /// unsaved-changes guard has already been resolved by the caller.
    fn do_new(&mut self) {
        if !self.can_replace_graph() {
            return;
        }
        if let Some(run) = &mut self.run {
            run.stop();
        }
        self.capture.clear_completed();
        self.run_message = None;
        self.error_badges.clear();
        self.node_graph.new_graph();
        self.restore_sampling_overlay_setting();
        self.restore_viewer_lane_order_setting();
        self.restore_panel_layout_setting();
        self.platform.derived_cache_nodes.clear();
        self.platform.current_file = None;
        self.mark_graph_saved();
        self.toasts.info("New graph");
    }

    /// Requests File → New, guarding on unsaved changes the same way
    /// `request_quit` does.
    fn request_new(&mut self) {
        if self.has_unsaved_changes() {
            self.platform.pending_guarded_action = Some(GuardedAction::New);
        } else {
            self.do_new();
        }
    }

    /// Requests loading `path` (e.g. from Open Recent), guarding on unsaved
    /// changes the same way `request_quit` does.
    fn request_load_path(&mut self, path: PathBuf) {
        if self.has_unsaved_changes() {
            self.platform.pending_guarded_action = Some(GuardedAction::LoadPath(path));
        } else {
            self.load_file(path);
        }
    }

    fn choose_and_load_file(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Load graph")
            .add_filter("Graph JSON", &["json"]);
        if let Some(parent) = self
            .platform
            .current_file
            .as_ref()
            .and_then(|path| path.parent())
        {
            dialog = dialog.set_directory(parent);
        }
        if let Some(path) = dialog.pick_file() {
            self.load_file(path);
        }
    }

    fn save_file(&mut self) -> bool {
        let Some(path) = self.platform.current_file.clone() else {
            return self.save_file_as();
        };
        self.save_to_file(path)
    }

    fn save_file_as(&mut self) -> bool {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save graph as")
            .set_file_name("pipeline.json")
            .add_filter("Graph JSON", &["json"]);
        if let Some(path) = &self.platform.current_file {
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

    fn save_to_file(&mut self, path: PathBuf) -> bool {
        if let Err(error) = self.sync_panel_layout_setting() {
            self.toasts
                .error(format!("Could not save the panel layout: {error}"));
            return false;
        }
        self.synchronize_payload_subscription_manifest(false);
        match self.node_graph.save_to_path(&path) {
            Ok(()) => {
                self.platform.current_file = Some(path.clone());
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

    fn choose_and_save_capture_data(&mut self) {
        let format = CaptureRawExportFormat::Portable;
        let descriptor = format.descriptor();
        let mut dialog = rfd::FileDialog::new()
            .set_title(descriptor.dialog_title)
            .set_file_name(descriptor.default_file_name)
            .add_filter(descriptor.label, &[descriptor.extension]);
        if let Some(parent) = self
            .platform
            .current_file
            .as_ref()
            .and_then(|path| path.parent())
        {
            dialog = dialog.set_directory(parent);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension(descriptor.extension);
        }
        if let Err(error) = self.capture.start_export_current(format, path) {
            self.toasts.error(error);
        }
    }

    fn mark_graph_saved(&mut self) {
        if let Err(error) = self.sync_panel_layout_setting() {
            self.toasts
                .error(format!("Could not save the panel layout: {error}"));
            return;
        }
        self.synchronize_payload_subscription_manifest(false);
        match self.node_graph.snapshot_value() {
            Ok(graph) => self.platform.saved_graph = graph,
            Err(error) => self.toasts.error(error),
        }
    }

    fn has_unsaved_changes(&mut self) -> bool {
        if self.sync_panel_layout_setting().is_err() {
            return true;
        }
        self.synchronize_payload_subscription_manifest(false);
        self.node_graph
            .snapshot_value()
            .map_or(true, |graph| graph != self.platform.saved_graph)
    }

    fn request_quit(&mut self, ctx: &egui::Context) {
        if self.has_unsaved_changes() {
            self.platform.pending_guarded_action = Some(GuardedAction::Quit);
        } else {
            self.platform.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn execute_file_command(&mut self, command: FileCommand, ctx: &egui::Context) {
        match command {
            FileCommand::New => self.request_new(),
            FileCommand::Load => self.choose_and_load_file(),
            FileCommand::LoadPath(path) => self.request_load_path(path),
            FileCommand::ClearRecent => self.platform.confirm_clear_recent = true,
            FileCommand::Save => {
                self.save_file();
            }
            FileCommand::SaveAs => {
                self.save_file_as();
            }
            FileCommand::SaveCaptureData => self.choose_and_save_capture_data(),
            FileCommand::Quit => self.request_quit(ctx),
        }
    }

    /// Resolves whatever `pending_guarded_action` (quit/new/load-over-dirty)
    /// is outstanding — Save/Don't Save/Cancel, same dialog for all three
    /// (Phase 5.1).
    fn show_guarded_action_dialog(&mut self, ctx: &egui::Context) {
        if self.platform.pending_guarded_action.is_none() {
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
            Some(DialogChoice::Cancel) => self.platform.pending_guarded_action = None,
            _ => {}
        }
    }

    fn complete_guarded_action(&mut self, ctx: &egui::Context) {
        let Some(action) = self.platform.pending_guarded_action.take() else {
            return;
        };
        match action {
            GuardedAction::Quit => {
                self.platform.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            GuardedAction::New => self.do_new(),
            GuardedAction::LoadPath(path) => self.load_file(path),
        }
    }

    /// Resolves the "Clear the recent files list?" confirmation triggered
    /// by either the egui or native "Clear Recent" menu item.
    fn show_clear_recent_dialog(&mut self, ctx: &egui::Context) {
        if !self.platform.confirm_clear_recent {
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
                self.platform.recent_files.clear();
                #[cfg(target_os = "macos")]
                notify_recent_files_changed(&[]);
                self.platform.confirm_clear_recent = false;
            }
            Some(DialogChoice::Cancel) => self.platform.confirm_clear_recent = false,
            None => {}
        }
    }

    fn show_clear_derived_caches_dialog(&mut self, ctx: &egui::Context) {
        if !self.platform.confirm_clear_derived_caches {
            return;
        }

        let mut clear = false;
        let mut cancel = false;
        egui::Window::new("Clear all derived data caches?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Cached decoded data for every pipeline will be removed.");
                ui.horizontal(|ui| {
                    if ui.button("Clear All").clicked() {
                        clear = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if clear {
            self.platform.confirm_clear_derived_caches = false;
            self.clear_all_derived_caches();
        } else if cancel {
            self.platform.confirm_clear_derived_caches = false;
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn show_menu_bar(&mut self, ui: &mut egui::Ui) {
        let shortcut = |action| {
            self.input_bindings
                .shortcut(&["global"], action)
                .unwrap_or_else(|| panic!("missing global.{action} input binding"))
        };
        let new_shortcut = shortcut("new");
        let load_shortcut = shortcut("open");
        let save_shortcut = shortcut("save");
        let save_as_shortcut = shortcut("save_as");
        let quit_shortcut = shortcut("quit");
        let run_shortcut = shortcut("run");
        let stop_shortcut = shortcut("stop");
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
                        .recent_files()
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
                let can_save_capture = self.capture.current_session_id().is_some()
                    && !self.capture.is_active()
                    && self.capture.export_status().is_none();
                if ui
                    .add_enabled(can_save_capture, egui::Button::new("Save Capture Data..."))
                    .on_disabled_hover_text("Finish a capture before saving its data")
                    .clicked()
                {
                    command = Some(FileCommand::SaveCaptureData);
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
            ui.menu_button("View", |ui| {
                for (label, content_id, icon) in self.available_view_panels() {
                    if icon.menu_item(ui, &label).clicked() {
                        self.show_view_panel(&content_id);
                        ui.close();
                    }
                }
                ui.separator();
                if panel_layout::PanelIcon::Reset
                    .menu_item(ui, "Reset Layout")
                    .clicked()
                {
                    self.reset_panel_layout();
                    ui.close();
                }
            });
            ui.menu_button("Pipeline", |ui| {
                let unavailable = self.run_unavailable_reason();
                let run = ui.add_enabled(
                    unavailable.is_none(),
                    egui::Button::new("Run").shortcut_text(ui.ctx().format_shortcut(&run_shortcut)),
                );
                if let Some(reason) = unavailable {
                    run.clone().on_disabled_hover_text(reason);
                }
                if run.clicked() {
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
                ui.separator();
                if ui
                    .add_enabled(
                        !self.is_running(),
                        egui::Button::new("Clear All Derived Data Caches..."),
                    )
                    .clicked()
                {
                    self.request_clear_all_derived_caches();
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button(format!("About {APPLICATION_NAME}")).clicked() {
                    self.about.open();
                    ui.close();
                }
            });
        });

        if let Some(command) = command {
            self.execute_file_command(command, ui.ctx());
        }
    }

    fn can_clear_derived_caches(&mut self) -> bool {
        if self.is_running() {
            self.toasts
                .error("Stop the pipeline before clearing derived data caches");
            false
        } else {
            true
        }
    }

    fn release_derived_data_handles(&mut self) {
        self.run = None;
        self.logic_analyzer
            .set_derived_lanes(signal_processing::DerivedLanes::new());
    }

    fn refresh_derived_cache_nodes(&mut self) {
        self.platform.derived_cache_nodes = compiler::derived_cache_configs_by_node(
            self.node_graph.graph(),
            &self.builders,
            &derived_cache_directory(),
        )
        .map(|inventory| {
            inventory
                .into_keys()
                .filter(|id| self.node_graph.graph().nodes.contains_key(id))
                .collect()
        })
        .unwrap_or_default();
    }

    fn clear_node_derived_cache(&mut self, node_id: NodeId) {
        if !self.can_clear_derived_caches() {
            return;
        }
        let node_name = self
            .node_graph
            .graph()
            .nodes
            .get(&node_id)
            .map(|node| node.title.clone())
            .unwrap_or_else(|| "node".to_owned());
        let configs = match compiler::derived_cache_configs_by_node(
            self.node_graph.graph(),
            &self.builders,
            &derived_cache_directory(),
        ) {
            Ok(mut inventory) => inventory.remove(&node_id).unwrap_or_default(),
            Err(errors) => {
                let message = errors
                    .first()
                    .map(|error| error.message.as_str())
                    .unwrap_or("graph could not be compiled");
                self.toasts
                    .error(format!("Cannot determine cache: {message}"));
                return;
            }
        };
        if configs.is_empty() {
            self.toasts
                .info(format!("No derived data cache found for {node_name}"));
            return;
        }

        self.release_derived_data_handles();
        let mut removed_entries = 0usize;
        let mut removed_bytes = 0u64;
        for config in &configs {
            match signal_processing::clear_cache_entry(config) {
                Ok(stats) => {
                    removed_entries += stats.removed_entries;
                    removed_bytes = removed_bytes.saturating_add(stats.removed_bytes);
                }
                Err(error) => {
                    self.toasts.error(format!("Failed to clear cache: {error}"));
                    return;
                }
            }
        }
        if removed_entries == 0 {
            self.toasts
                .info(format!("No derived data cache found for {node_name}"));
        } else {
            self.toasts.info(format!(
                "Cleared {removed_entries} derived cache entr{} for {node_name} ({removed_bytes} bytes)",
                if removed_entries == 1 { "y" } else { "ies" }
            ));
        }
    }

    fn request_clear_all_derived_caches(&mut self) {
        if self.can_clear_derived_caches() {
            self.platform.confirm_clear_derived_caches = true;
        }
    }

    fn clear_all_derived_caches(&mut self) {
        if !self.can_clear_derived_caches() {
            return;
        }
        self.release_derived_data_handles();
        let directory = derived_cache_directory();
        match signal_processing::clear_cache(&directory) {
            Ok(stats) if stats.removed_entries == 0 && stats.removed_bytes == 0 => {
                self.toasts.info("No derived data caches found");
            }
            Ok(stats) => self.toasts.info(format!(
                "Cleared {} derived cache entr{} ({} bytes)",
                stats.removed_entries,
                if stats.removed_entries == 1 {
                    "y"
                } else {
                    "ies"
                },
                stats.removed_bytes
            )),
            Err(error) => self
                .toasts
                .error(format!("Failed to clear caches: {error}")),
        }
    }
}
