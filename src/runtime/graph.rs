//! Graph builder for constructing streaming node graphs
//!
//! Provides a builder API for connecting nodes with typed channels.

use std::any::TypeId;
use std::collections::HashMap;

/// Unique identifier for a node in the graph
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(usize);

impl NodeId {
    pub fn new(id: usize) -> Self {
        Self(id)
    }

    pub fn as_usize(&self) -> usize {
        self.0
    }
}

/// Represents a connection between two nodes
#[derive(Debug, Clone)]
pub struct Connection {
    pub from_node: NodeId,
    pub from_port: usize,
    pub to_node: NodeId,
    pub to_port: usize,
    pub buffer_size: usize,
    pub type_id: TypeId,
}

/// Builder for constructing a streaming graph
pub struct GraphBuilder {
    next_node_id: usize,
    nodes: HashMap<NodeId, NodeInfo>,
    connections: Vec<Connection>,
}

struct NodeInfo {
    name: String,
    input_ports: Vec<PortInfo>,
    output_ports: Vec<PortInfo>,
}

#[derive(Clone)]
struct PortInfo {
    type_id: TypeId,
    type_name: String,
}

impl GraphBuilder {
    /// Create a new graph builder
    pub fn new() -> Self {
        Self {
            next_node_id: 0,
            nodes: HashMap::new(),
            connections: Vec::new(),
        }
    }

    /// Add a processing node (inputs and outputs)
    pub fn add_process_node(
        &mut self,
        name: impl Into<String>,
        input_types: Vec<(TypeId, String)>,
        output_types: Vec<(TypeId, String)>,
    ) -> NodeId {
        let id = NodeId::new(self.next_node_id);
        self.next_node_id += 1;

        let input_ports = input_types
            .into_iter()
            .map(|(type_id, type_name)| PortInfo { type_id, type_name })
            .collect();

        let output_ports = output_types
            .into_iter()
            .map(|(type_id, type_name)| PortInfo { type_id, type_name })
            .collect();

        self.nodes.insert(
            id,
            NodeInfo {
                name: name.into(),
                input_ports,
                output_ports,
            },
        );

        id
    }

    /// Connect two nodes with a typed channel
    pub fn connect<T: Send + 'static>(
        &mut self,
        from_node: NodeId,
        from_port: usize,
        to_node: NodeId,
        to_port: usize,
        buffer_size: usize,
    ) -> Result<(), String> {
        // Validate nodes exist
        let from_info = self
            .nodes
            .get(&from_node)
            .ok_or_else(|| format!("Source node {:?} not found", from_node))?;
        let to_info = self
            .nodes
            .get(&to_node)
            .ok_or_else(|| format!("Destination node {:?} not found", to_node))?;

        // Validate ports exist
        let from_port_info = from_info.output_ports.get(from_port).ok_or_else(|| {
            format!(
                "Source port {} not found on node {}",
                from_port, from_info.name
            )
        })?;
        let to_port_info = to_info.input_ports.get(to_port).ok_or_else(|| {
            format!(
                "Destination port {} not found on node {}",
                to_port, to_info.name
            )
        })?;

        // Validate types match
        let expected_type = TypeId::of::<T>();
        if from_port_info.type_id != expected_type {
            return Err(format!(
                "Source port type mismatch: expected {}, got {}",
                std::any::type_name::<T>(),
                from_port_info.type_name
            ));
        }
        if to_port_info.type_id != expected_type {
            return Err(format!(
                "Destination port type mismatch: expected {}, got {}",
                std::any::type_name::<T>(),
                to_port_info.type_name
            ));
        }

        self.connections.push(Connection {
            from_node,
            from_port,
            to_node,
            to_port,
            buffer_size,
            type_id: expected_type,
        });

        Ok(())
    }

    /// Get information about a node
    pub fn node_info(&self, node_id: NodeId) -> Option<(&str, usize, usize)> {
        self.nodes.get(&node_id).map(|info| {
            (
                info.name.as_str(),
                info.input_ports.len(),
                info.output_ports.len(),
            )
        })
    }

    /// Get all connections in the graph
    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    /// Get the number of nodes
    pub fn num_nodes(&self) -> usize {
        self.nodes.len()
    }

    /// Validate the graph (check all ports are connected, no cycles for now)
    pub fn validate(&self) -> Result<(), String> {
        // For now, just ensure inputs and outputs are properly connected
        // More sophisticated validation (cycle checking, etc.) can be added later

        for (node_id, node_info) in &self.nodes {
            // Check that all input ports are connected
            for input_port in 0..node_info.input_ports.len() {
                let connected = self
                    .connections
                    .iter()
                    .any(|conn| conn.to_node == *node_id && conn.to_port == input_port);

                if !connected {
                    return Err(format!(
                        "Input port {} on node '{}' is not connected",
                        input_port, node_info.name
                    ));
                }
            }

            // Note: Output ports don't need to be connected (e.g., for debugging taps)
        }

        Ok(())
    }
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_graph_building() {
        let mut builder = GraphBuilder::new();

        let source = builder.add_process_node(
            "source",
            vec![],
            vec![(TypeId::of::<u32>(), "u32".to_string())],
        );
        let sink = builder.add_process_node(
            "sink",
            vec![(TypeId::of::<u32>(), "u32".to_string())],
            vec![],
        );

        assert!(builder.connect::<u32>(source, 0, sink, 0, 1000).is_ok());
        assert!(builder.validate().is_ok());
    }

    #[test]
    fn test_type_mismatch() {
        let mut builder = GraphBuilder::new();

        let source = builder.add_process_node(
            "source",
            vec![],
            vec![(TypeId::of::<u32>(), "u32".to_string())],
        );
        let sink = builder.add_process_node(
            "sink",
            vec![(TypeId::of::<u64>(), "u64".to_string())],
            vec![],
        );

        assert!(builder.connect::<u32>(source, 0, sink, 0, 1000).is_err());
    }

    #[test]
    fn test_invalid_port() {
        let mut builder = GraphBuilder::new();

        let source = builder.add_process_node(
            "source",
            vec![],
            vec![(TypeId::of::<u32>(), "u32".to_string())],
        );
        let sink = builder.add_process_node(
            "sink",
            vec![(TypeId::of::<u32>(), "u32".to_string())],
            vec![],
        );

        // Try to connect to non-existent port
        assert!(builder.connect::<u32>(source, 1, sink, 0, 1000).is_err());
    }
}
