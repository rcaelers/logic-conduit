use super::{CapturePreviewSignal, demo_capture_source};

/// Finds an in-memory raw-capture preview supplied by a concrete source node.
pub fn capture_preview(
    graph: &node_graph::GraphState,
) -> Option<(node_graph::NodeId, Vec<CapturePreviewSignal>)> {
    graph.nodes.iter().find_map(|(&id, node)| {
        demo_capture_source::capture_preview(node).map(|preview| (id, preview))
    })
}
