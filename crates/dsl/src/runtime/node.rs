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
    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize>;
}
