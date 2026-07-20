pub(crate) struct PlatformState {
    pub(crate) capture_presentation_identity: Option<String>,
}

impl PlatformState {
    pub(crate) fn restore(
        _cc: &eframe::CreationContext,
        _widget: &mut node_graph::NodeGraphWidget,
    ) -> (Self, Option<panel_layout::PanelLayoutState>, f32) {
        (
            Self {
                capture_presentation_identity: None,
            },
            None,
            // Four additional 30px waveform rows on a typical browser
            // viewport, so all demo-derived lanes remain visible after Run.
            0.50,
        )
    }

    pub(crate) fn recent_files(&self) -> &[std::path::PathBuf] {
        &[]
    }
}
