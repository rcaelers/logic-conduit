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

use super::edge_query::EdgeQuery;
use super::protocol::ProtocolKind;
use std::sync::Arc;

#[derive(Clone)]
pub struct InputProtocolCandidate {
    pub offered: Vec<ProtocolKind>,
    pub edge_query: Option<Arc<dyn EdgeQuery>>,
}

/// A configuration value delivered to a running node (live reconfiguration,
/// `docs/APP_DESIGN.md`). Deliberately a tiny bespoke type: the runtime crate stays
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

    /// Selects one transport per input after producers have exposed their
    /// actual capabilities and optional query metadata. The default keeps
    /// producer preference order. Stateful consumers may override this to
    /// make one coordinated choice across a group of inputs.
    fn select_input_protocols(
        &self,
        candidates: &[Option<InputProtocolCandidate>],
    ) -> Vec<Option<ProtocolKind>> {
        let schemas = self.input_schema();
        candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let candidate = candidate.as_ref()?;
                let accepted = &schemas.get(index)?.protocols;
                candidate
                    .offered
                    .iter()
                    .find(|protocol| accepted.contains(protocol))
                    .copied()
            })
            .collect()
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

    /// Random-access query handle for output port `port`, if this node
    /// can answer it without streaming. Only called by `Pipeline::build`
    /// for connections that negotiated
    /// [`ProtocolKind::EdgeQuery`](super::protocol::ProtocolKind::EdgeQuery)
    /// (see [`PortSchema::protocols`](super::ports::PortSchema::protocols)).
    /// `input_queries` carries this node's own inputs' negotiated query
    /// handles (in `input_schema()` order, `None` where a given input
    /// didn't negotiate `EdgeQuery`) — empty today since only zero-input
    /// source nodes implement this, but a future pass-through node
    /// (e.g. a logic gate) would compose its output's answer from these.
    /// Default: unsupported.
    fn edge_query(
        &self,
        _port: usize,
        _input_queries: &[Option<Arc<dyn EdgeQuery>>],
    ) -> Option<Arc<dyn EdgeQuery>> {
        None
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
    fn select_input_protocols(
        &self,
        candidates: &[Option<InputProtocolCandidate>],
    ) -> Vec<Option<ProtocolKind>> {
        (**self).select_input_protocols(candidates)
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
    fn edge_query(
        &self,
        port: usize,
        input_queries: &[Option<Arc<dyn EdgeQuery>>],
    ) -> Option<Arc<dyn EdgeQuery>> {
        (**self).edge_query(port, input_queries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ports::{PortDirection, PortSchema};

    struct StreamSelectingNode;

    impl ProcessNode for StreamSelectingNode {
        fn name(&self) -> &str {
            "stream_selector"
        }

        fn num_inputs(&self) -> usize {
            1
        }

        fn num_outputs(&self) -> usize {
            0
        }

        fn input_schema(&self) -> Vec<PortSchema> {
            vec![
                PortSchema::new::<u8>("input", 0, PortDirection::Input)
                    .with_protocols(vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream]),
            ]
        }

        fn select_input_protocols(
            &self,
            candidates: &[Option<InputProtocolCandidate>],
        ) -> Vec<Option<ProtocolKind>> {
            candidates
                .iter()
                .map(|candidate| candidate.as_ref().map(|_| ProtocolKind::Stream))
                .collect()
        }

        fn work(&mut self, _inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            unreachable!()
        }
    }

    #[test]
    fn boxed_process_node_forwards_protocol_selection() {
        let node: Box<dyn ProcessNode> = Box::new(StreamSelectingNode);
        let selected = node.select_input_protocols(&[Some(InputProtocolCandidate {
            offered: vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream],
            edge_query: None,
        })]);

        assert_eq!(selected, vec![Some(ProtocolKind::Stream)]);
    }
}
