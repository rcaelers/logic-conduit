pub(crate) struct PlatformState {
    pub(crate) preview_source: Option<node_graph::NodeId>,
}

impl PlatformState {
    pub(crate) fn restore(
        _cc: &eframe::CreationContext,
        widget: &mut node_graph::NodeGraphWidget,
    ) -> (Self, Option<panel_layout::PanelLayoutState>, f32) {
        let graph: node_graph::GraphState =
            serde_json::from_str(include_str!("../../../../graphs/wasm_decoder_demo.json"))
                .expect("checked-in wasm decoder demo graph is valid");
        widget.set_graph(graph);
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
