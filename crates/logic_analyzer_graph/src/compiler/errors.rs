use node_graph::NodeId;

/// An error found while lowering the editor graph.
#[derive(Debug, Clone)]
pub struct CompileError {
    /// Offending node, for editor badges; `None` for graph-level errors.
    pub node: Option<NodeId>,
    pub message: String,
}

impl CompileError {
    pub(crate) fn on(node: NodeId, message: impl Into<String>) -> Self {
        Self {
            node: Some(node),
            message: message.into(),
        }
    }

    pub(crate) fn global(message: impl Into<String>) -> Self {
        Self {
            node: None,
            message: message.into(),
        }
    }
}

/// An error applying an edited graph to a live run.
#[derive(Debug)]
pub enum ApplyError {
    /// The edited graph does not lower; the running pipeline is untouched.
    Compile(Vec<CompileError>),
    /// The edit class cannot be applied live (source changed/replaced); the running
    /// pipeline is untouched — stop and rerun to pick it up.
    NeedsFullRestart(String),
    /// A live edit failed midway (e.g. a node failed to build).
    Apply(String),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compile(errors) => {
                write!(
                    formatter,
                    "edited graph has {} compile error(s)",
                    errors.len()
                )
            }
            Self::NeedsFullRestart(message) => {
                write!(formatter, "live edit requires a full restart: {message}")
            }
            Self::Apply(message) => write!(formatter, "could not apply live edit: {message}"),
        }
    }
}

impl std::error::Error for ApplyError {}
