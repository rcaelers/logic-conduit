//! Node trait for streaming processing
//!
//! Defines the ProcessNode trait that all streaming nodes must implement.
//! Nodes actively process data when work() is called by the scheduler.

// Re-export error types for backward compatibility
pub use super::errors::{WorkError, WorkResult};

// Re-export port types (now defined in ports module)
pub use super::ports::{InputPort, OutputPort};

// Re-export channel types (now defined in sender/receiver modules)
pub use super::receiver::Receiver;
pub use super::sender::Sender;

/// A configuration value delivered to a running node (live reconfiguration,
/// design §6.2). Deliberately a tiny bespoke type: the runtime crate stays
/// serde-free and nodes match on plain fields.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValue {
    U64(u64),
    I64(i64),
    Bool(bool),
    Text(String),
}

/// Named configuration fields for [`ProcessNode::apply_config`]; produced by
/// the app-layer builders that know how UI state maps onto runtime knobs.
pub type NodeConfig = std::collections::HashMap<String, ConfigValue>;

/// Outcome of a hot configuration attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigOutcome {
    /// The change is in effect from the next `work()` on.
    Applied,
    /// The node cannot apply this change while running; the supervisor
    /// restarts it in place.
    NeedsRestart,
}

/// A processing node that transforms data
/// - Sources have 0 inputs and N outputs
/// - Sinks have N inputs and 0 outputs
/// - Processors have N inputs and M outputs
pub trait ProcessNode: Send {
    /// Get a debug name for this node
    fn name(&self) -> &str;

    /// Check if this node should stop processing
    fn should_stop(&self) -> bool {
        false
    }

    /// Returns true if this node spawns its own worker threads and manages them internally.
    /// If true, the scheduler will call work() once to start the node, then wait for should_stop().
    /// If false (default), the scheduler will call work() repeatedly in a loop.
    fn is_self_threading(&self) -> bool {
        false
    }

    /// Number of input ports this node requires
    fn num_inputs(&self) -> usize;

    /// Number of output ports this node provides
    fn num_outputs(&self) -> usize;

    /// Get schema for all input ports (name + type + index)
    /// Default implementation returns empty list for backward compatibility
    fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        Vec::new()
    }

    /// Get schema for all output ports (name + type + index)
    /// Default implementation returns empty list for backward compatibility
    fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        Vec::new()
    }

    /// Get node type identifier for serialization
    /// Defaults to node name
    fn node_type(&self) -> &str {
        self.name()
    }

    /// Do work: read from inputs, process, write to outputs
    /// The scheduler provides references to input and output port slices
    /// Returns Ok(n) where n is the number of items produced, or Err on failure
    ///
    /// **Cooperative-backend invariant:** implementations must not send more
    /// than one item per output per `work()` call. `CooperativeManager`
    /// (used on wasm) only checks *before* calling `work()` that no output
    /// would currently block (`runtime::cooperative_manager`'s module doc);
    /// a node that fans out several sends to the same output within one
    /// call can still fill that output's channel mid-call and hit a real
    /// blocking `send()` — which, on that single-threaded scheduler,
    /// deadlocks the whole pump loop permanently. `PipelineManager`
    /// (thread-per-node, native) has no such constraint — blocking there
    /// only stalls the one node's own thread.
    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize>;

    /// Apply a configuration change while running (between `work()` calls).
    /// The default declines, telling the supervisor to restart the node
    /// in place with a freshly built instance.
    fn apply_config(&mut self, _config: &NodeConfig) -> ConfigOutcome {
        ConfigOutcome::NeedsRestart
    }
}

/// Forwarding impl so factories (e.g. the graph compiler) can hand
/// `Box<dyn ProcessNode>` to `Pipeline::add_process`.
impl ProcessNode for Box<dyn ProcessNode> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn should_stop(&self) -> bool {
        (**self).should_stop()
    }
    fn is_self_threading(&self) -> bool {
        (**self).is_self_threading()
    }
    fn num_inputs(&self) -> usize {
        (**self).num_inputs()
    }
    fn num_outputs(&self) -> usize {
        (**self).num_outputs()
    }
    fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        (**self).input_schema()
    }
    fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        (**self).output_schema()
    }
    fn node_type(&self) -> &str {
        (**self).node_type()
    }
    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        (**self).work(inputs, outputs)
    }
    fn apply_config(&mut self, config: &NodeConfig) -> ConfigOutcome {
        (**self).apply_config(config)
    }
}
