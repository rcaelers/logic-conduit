pub(crate) struct PlatformState {
    pub(crate) capture_presentation_identity: Option<String>,
}

impl PlatformState {
    pub(crate) fn restore(
        cc: &eframe::CreationContext,
        widget: &mut node_graph::NodeGraphWidget,
    ) -> (
        Self,
        Option<panel_layout::PanelLayoutState>,
        f32,
        crate::decoder_panel::DecoderPanelsState,
    ) {
        let restored = cc.storage.and_then(|storage| {
            eframe::get_value::<super::PersistedUiState>(storage, eframe::APP_KEY)
        });
        let (panel_layout, analyzer_split, decoder_panels) = restored
            .map(|state| state.restore(widget))
            .unwrap_or_else(|| {
                (
                    None,
                    // Four additional 30px waveform rows on a typical browser
                    // viewport, so all demo-derived lanes remain visible after Run.
                    0.50,
                    Default::default(),
                )
            });
        (
            Self {
                capture_presentation_identity: None,
            },
            panel_layout,
            analyzer_split,
            decoder_panels,
        )
    }

    pub(crate) fn recent_files(&self) -> &[std::path::PathBuf] {
        &[]
    }
}
