use node_graph::NodeId;

/// An error found while lowering the editor graph.
#[derive(Debug, Clone)]
pub struct CompileError {
    /// Offending node, for editor badges; `None` for graph-level errors.
    pub node: Option<NodeId>,
    pub message: String,
}

impl CompileError {
    pub(super) fn on(node: NodeId, message: impl Into<String>) -> Self {
        Self {
            node: Some(node),
            message: message.into(),
        }
    }

    pub(super) fn global(message: impl Into<String>) -> Self {
        Self {
            node: None,
            message: message.into(),
        }
    }
}

/// An error applying an edited graph to a live run.
#[derive(Debug)]
#[allow(dead_code)] // payloads carried for logs/tests
pub enum ApplyError {
    /// The edited graph does not lower; the running pipeline is untouched.
    Compile(Vec<CompileError>),
    /// The edit class cannot be applied live (source changed/replaced); the running
    /// pipeline is untouched — stop and rerun to pick it up.
    NeedsFullRestart(String),
    /// A live edit failed midway (e.g. a node failed to build).
    Apply(String),
}
