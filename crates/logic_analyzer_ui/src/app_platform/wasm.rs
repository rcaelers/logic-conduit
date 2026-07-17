pub(crate) struct PlatformState {
    pub(crate) preview_source: Option<node_graph::NodeId>,
}

impl PlatformState {
    pub(crate) fn restore(
        _cc: &eframe::CreationContext,
        widget: &mut node_graph::NodeGraphWidget,
    ) -> (Self, Option<panel_layout::PanelLayoutState>, f32) {
        logic_analyzer_graph::nodes::populate_binary_decoder_demo(widget);
        (
            Self {
                preview_source: None,
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
