pub(crate) struct PlatformState {
    pub(crate) capture_presentation_identity: Option<String>,
}

impl PlatformState {
    pub(crate) fn restore(
        cc: &eframe::CreationContext,
        widget: &mut node_graph::NodeGraphWidget,
    ) -> Self {
        if let Some(restored) = cc.storage.and_then(|storage| {
            eframe::get_value::<super::PersistedUiState>(storage, eframe::APP_KEY)
        }) {
            restored.restore(widget);
        }
        Self {
            capture_presentation_identity: None,
        }
    }

    pub(crate) fn recent_files(&self) -> &[std::path::PathBuf] {
        &[]
    }
}
