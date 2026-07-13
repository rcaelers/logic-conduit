pub(crate) struct PlatformState;

impl PlatformState {
    pub(crate) fn restore(
        _cc: &eframe::CreationContext,
        widget: &mut node_graph::NodeGraphWidget,
    ) -> (Self, f32) {
        crate::nodes::populate_uart_demo(widget);
        (Self, 0.42)
    }

    pub(crate) fn recent_files(&self) -> &[std::path::PathBuf] {
        &[]
    }
}
