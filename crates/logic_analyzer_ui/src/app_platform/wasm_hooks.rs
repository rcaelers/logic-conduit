use super::*;

impl App {
    pub(super) fn platform_load_startup_file(&mut self, _file: Option<&std::path::Path>) {}

    pub(super) fn platform_prepare_run(&mut self, _ctx: &mut compiler::CompileCtx) {}

    pub(super) fn platform_raw_input_hook(
        &mut self,
        _ctx: &egui::Context,
        _raw_input: &mut egui::RawInput,
    ) {
    }

    pub(super) fn platform_logic(&mut self, _ctx: &egui::Context) {}

    pub(super) fn platform_save(&mut self, _storage: &mut dyn eframe::Storage) {}

    pub(super) fn platform_before_ui(&mut self, ui: &mut egui::Ui) {
        let shortcut = |action| {
            self.input_bindings
                .shortcut(&["global"], action)
                .unwrap_or_else(|| panic!("missing global.{action} input binding"))
        };
        let run_shortcut = shortcut("run");
        let stop_shortcut = shortcut("stop");

        if ui.input_mut(|input| input.consume_shortcut(&run_shortcut)) {
            self.run_command();
        } else if ui.input_mut(|input| input.consume_shortcut(&stop_shortcut)) {
            self.stop_command();
        }

        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("View", |ui| {
                for (label, content_id, icon) in [
                    ("Watches", "watches", panel_layout::PanelIcon::List),
                    ("Triggers", "triggers", panel_layout::PanelIcon::Target),
                    ("Decoder", "decoder", panel_layout::PanelIcon::Table),
                ] {
                    if icon.menu_item(ui, label).clicked() {
                        self.show_view_panel(content_id);
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
                    egui::Button::new("Run")
                        .shortcut_text(ui.ctx().format_shortcut(&run_shortcut)),
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
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About DSL Pipeline Editor").clicked() {
                    self.about.open();
                    ui.close();
                }
            });
        });
    }

    pub(super) fn platform_sync_capture(&mut self) {
        if self.logic_analyzer.has_growing_capture() {
            return;
        }
        let preview = nodes::capture_preview(self.node_graph.graph());
        let source = preview.as_ref().map(|(id, _)| *id);
        if source == self.platform.preview_source {
            return;
        }
        self.platform.preview_source = source;
        match preview {
            Some((_, signals)) => self.set_capture_preview(signals),
            None => self.logic_analyzer.clear_capture(),
        }
    }

    pub(super) fn platform_restore_graph_capture(&mut self) {
        self.platform.preview_source = None;
    }

    pub(super) fn platform_before_graph(&mut self) {}

    pub(super) fn platform_after_graph(&mut self) {}

    pub(super) fn platform_after_ui(&mut self, _ctx: &egui::Context) {}
}
