use logic_analyzer_graph::host as compiler;
use logic_analyzer_graph_api::node_support::CapturePresentation;

use crate::app::App;
use crate::product::APPLICATION_NAME;

impl App {
    pub(crate) fn platform_clear_capture_caches(
        &mut self,
        _configs: &[signal_processing::PersistentStoreConfig],
    ) -> Result<(), String> {
        Ok(())
    }

    pub(crate) fn platform_load_startup_file(&mut self, _file: Option<&std::path::Path>) {}

    pub(crate) fn platform_prepare_run(&mut self, _ctx: &mut compiler::CompileCtx) {}

    pub(crate) fn platform_raw_input_hook(
        &mut self,
        _ctx: &egui::Context,
        _raw_input: &mut egui::RawInput,
    ) {
    }

    pub(crate) fn platform_logic(&mut self, _ctx: &egui::Context) {}

    pub(crate) fn platform_save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Err(error) = self.sync_panel_layout_setting() {
            self.toasts
                .error(format!("Could not update the graph panel layout: {error}"));
        }
        let state = super::PersistedUiState::capture(self.node_graph.ui_prefs());
        eframe::set_value(storage, eframe::APP_KEY, &state);
    }

    pub(crate) fn platform_before_ui(&mut self, ui: &mut egui::Ui) {
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
            });
            ui.menu_button("Help", |ui| {
                if ui.button(format!("About {APPLICATION_NAME}")).clicked() {
                    self.about.open();
                    ui.close();
                }
            });
        });
    }

    pub(crate) fn platform_sync_capture(&mut self) {
        if self.logic_analyzer.has_growing_capture() {
            return;
        }
        let presentation = self
            .graph_compiler
            .discover_capture_presentation(self.node_graph.graph())
            .ok()
            .flatten();
        let identity = presentation.as_ref().map(|value| value.identity.as_str());
        if identity == self.platform.capture_presentation_identity.as_deref() {
            return;
        }
        self.platform.capture_presentation_identity = identity.map(str::to_owned);
        match presentation.map(|value| value.presentation) {
            Some(CapturePresentation::InMemory { signals, .. }) => {
                self.set_capture_preview(signals)
            }
            Some(CapturePresentation::Channels(channels)) => {
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
            Some(CapturePresentation::Indexed { .. }) | None => self.logic_analyzer.clear_capture(),
        }
    }

    pub(crate) fn platform_restore_graph_capture(&mut self) {
        self.platform.capture_presentation_identity = None;
    }

    pub(crate) fn platform_before_graph(&mut self) {}

    pub(crate) fn platform_after_graph(&mut self) {}

    pub(crate) fn platform_after_ui(&mut self, _ctx: &egui::Context) {}
}
