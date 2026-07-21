#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct PersistedUiState {
    pub(crate) graph_ui_prefs: node_graph::GraphUiPrefs,
}

impl PersistedUiState {
    pub(crate) fn capture(graph_ui_prefs: node_graph::GraphUiPrefs) -> Self {
        Self { graph_ui_prefs }
    }

    pub(crate) fn restore(self, widget: &mut node_graph::NodeGraphWidget) {
        widget.set_ui_prefs(self.graph_ui_prefs);
    }
}
