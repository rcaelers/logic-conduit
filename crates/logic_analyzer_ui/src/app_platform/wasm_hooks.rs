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

    pub(super) fn platform_before_ui(&mut self, _ui: &mut egui::Ui) {}

    pub(super) fn platform_sync_capture(&mut self) {
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

    pub(super) fn platform_before_graph(&mut self) {}

    pub(super) fn platform_after_graph(&mut self) {}

    pub(super) fn platform_after_ui(&mut self, _ctx: &egui::Context) {}
}
