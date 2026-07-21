#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct PersistedUiState {
    pub(crate) analyzer_split: f32,
    #[serde(default)]
    pub(crate) panel_layout: Option<panel_layout::PanelLayoutState>,
    pub(crate) graph_ui_prefs: node_graph::GraphUiPrefs,
    #[serde(default)]
    pub(crate) decoder_panels: crate::decoder_panel::DecoderPanelsState,
}

impl PersistedUiState {
    pub(crate) fn capture(
        analyzer_split: f32,
        panel_layout: panel_layout::PanelLayoutState,
        graph_ui_prefs: node_graph::GraphUiPrefs,
        decoder_panels: crate::decoder_panel::DecoderPanelsState,
    ) -> Self {
        Self {
            analyzer_split,
            panel_layout: Some(panel_layout),
            graph_ui_prefs,
            decoder_panels,
        }
    }

    pub(crate) fn restore(
        self,
        widget: &mut node_graph::NodeGraphWidget,
    ) -> (
        Option<panel_layout::PanelLayoutState>,
        f32,
        crate::decoder_panel::DecoderPanelsState,
    ) {
        widget.set_ui_prefs(self.graph_ui_prefs);
        (self.panel_layout, self.analyzer_split, self.decoder_panels)
    }
}
