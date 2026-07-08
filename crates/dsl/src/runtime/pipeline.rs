//! Pipeline builder for constructing node graphs

use super::edge_query::EdgeQuery;
use super::errors::ConnectionError;
use super::node::{InputPort, OutputPort, ProcessNode};
use super::ports::PortSchema;
use super::protocol::ProtocolKind;
use super::scheduler::Scheduler;
use super::type_registry::TYPE_REGISTRY;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info};

/// Pipeline builder that manages nodes and connections
pub struct Pipeline {
    nodes: Vec<(usize, Box<dyn ProcessNode>)>,
    node_names: HashMap<String, usize>,
    node_schemas: HashMap<usize, (Vec<PortSchema>, Vec<PortSchema>)>,
    connections: Vec<PendingConnection>,
    next_id: usize,
    default_buffer_size: usize,
}

pub(crate) struct PendingConnection {
    pub(crate) from_node: usize,
    pub(crate) from_port: usize,
    pub(crate) to_node: usize,
    pub(crate) to_port: usize,
    pub(crate) type_id: TypeId,
    pub(crate) buffer_size: usize,
}

impl Pipeline {
    /// Create a new pipeline
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            node_names: HashMap::new(),
            node_schemas: HashMap::new(),
            connections: Vec::new(),
            next_id: 0,
            default_buffer_size: 1000,
        }
    }

    /// Set the default buffer size for connections
    pub fn with_default_buffer_size(mut self, size: usize) -> Self {
        self.default_buffer_size = size;
        self
    }

    /// Add a process node by name (inputs/outputs determined automatically from node)
    pub fn add_process<N: ProcessNode + 'static>(
        &mut self,
        name: impl Into<String>,
        node: N,
    ) -> Result<(), String> {
        let name = name.into();

        if self.node_names.contains_key(&name) {
            return Err(format!("Node with name '{}' already exists", name));
        }

        let input_schemas = node.input_schema();
        let output_schemas = node.output_schema();

        let id = self.next_id;
        self.next_id += 1;

        self.node_schemas
            .insert(id, (input_schemas.clone(), output_schemas.clone()));
        self.node_names.insert(name, id);
        self.nodes.push((id, Box::new(node)));

        Ok(())
    }

    /// Connect two nodes by name and port name
    pub fn connect(
        &mut self,
        from_node: &str,
        from_port: &str,
        to_node: &str,
        to_port: &str,
    ) -> Result<(), Box<ConnectionError>> {
        self.connect_with_buffer(
            from_node,
            from_port,
            to_node,
            to_port,
            self.default_buffer_size,
        )
    }

    /// Connect with custom buffer size
    pub fn connect_with_buffer(
        &mut self,
        from_node: &str,
        from_port: &str,
        to_node: &str,
        to_port: &str,
        buffer_size: usize,
    ) -> Result<(), Box<ConnectionError>> {
        // Look up node IDs
        let from_id = *self
            .node_names
            .get(from_node)
            .ok_or_else(|| Box::new(ConnectionError::NodeNotFound(from_node.to_string())))?;
        let to_id = *self
            .node_names
            .get(to_node)
            .ok_or_else(|| Box::new(ConnectionError::NodeNotFound(to_node.to_string())))?;

        // Get schemas
        let (_, from_outputs) = self
            .node_schemas
            .get(&from_id)
            .ok_or_else(|| Box::new(ConnectionError::NodeNotFound(from_node.to_string())))?;
        let (to_inputs, _) = self
            .node_schemas
            .get(&to_id)
            .ok_or_else(|| Box::new(ConnectionError::NodeNotFound(to_node.to_string())))?;

        // Find output port
        let from_schema = from_outputs
            .iter()
            .find(|s| s.name == from_port)
            .ok_or_else(|| {
                Box::new(ConnectionError::PortNotFound {
                    node: from_node.to_string(),
                    port: from_port.to_string(),
                })
            })?;

        // Find input port
        let to_schema = to_inputs
            .iter()
            .find(|s| s.name == to_port)
            .ok_or_else(|| {
                Box::new(ConnectionError::PortNotFound {
                    node: to_node.to_string(),
                    port: to_port.to_string(),
                })
            })?;

        // Type check
        if from_schema.type_id != to_schema.type_id {
            return Err(Box::new(ConnectionError::TypeMismatch {
                from_node: from_node.to_string(),
                from_port: from_port.to_string(),
                from_type: from_schema.type_id,
                to_node: to_node.to_string(),
                to_port: to_port.to_string(),
                to_type: to_schema.type_id,
            }));
        }

        // Check for duplicate connection to same input port
        if self
            .connections
            .iter()
            .any(|c| c.to_node == to_id && c.to_port == to_schema.index)
        {
            return Err(Box::new(ConnectionError::DuplicateConnection(format!(
                "Input port '{}' on node '{}' is already connected",
                to_port, to_node
            ))));
        }

        // Store connection with custom buffer size
        self.connections.push(PendingConnection {
            from_node: from_id,
            from_port: from_schema.index,
            to_node: to_id,
            to_port: to_schema.index,
            type_id: from_schema.type_id,
            buffer_size,
        });

        Ok(())
    }

    /// Get input port schema for a node by name
    pub fn get_node_input_schema(&self, name: &str, port: &str) -> Result<&PortSchema, String> {
        let id = self
            .node_names
            .get(name)
            .ok_or_else(|| format!("Node '{}' not found", name))?;
        let (inputs, _) = self
            .node_schemas
            .get(id)
            .ok_or_else(|| format!("Node '{}' not found", name))?;
        inputs
            .iter()
            .find(|s| s.name == port)
            .ok_or_else(|| format!("Input port '{}' not found on node '{}'", port, name))
    }

    /// Get output port schema for a node by name
    pub fn get_node_output_schema(&self, name: &str, port: &str) -> Result<&PortSchema, String> {
        let id = self
            .node_names
            .get(name)
            .ok_or_else(|| format!("Node '{}' not found", name))?;
        let (_, outputs) = self
            .node_schemas
            .get(id)
            .ok_or_else(|| format!("Node '{}' not found", name))?;
        outputs
            .iter()
            .find(|s| s.name == port)
            .ok_or_else(|| format!("Output port '{}' not found on node '{}'", port, name))
    }

    /// List all input ports for a node by name
    pub fn list_node_inputs(&self, name: &str) -> Result<&[PortSchema], String> {
        let id = self
            .node_names
            .get(name)
            .ok_or_else(|| format!("Node '{}' not found", name))?;
        let (inputs, _) = self
            .node_schemas
            .get(id)
            .ok_or_else(|| format!("Node '{}' not found", name))?;
        Ok(inputs.as_slice())
    }

    /// List all output ports for a node by name
    pub fn list_node_outputs(&self, name: &str) -> Result<&[PortSchema], String> {
        let id = self
            .node_names
            .get(name)
            .ok_or_else(|| format!("Node '{}' not found", name))?;
        let (_, outputs) = self
            .node_schemas
            .get(id)
            .ok_or_else(|| format!("Node '{}' not found", name))?;
        Ok(outputs.as_slice())
    }

    /// List all node names
    pub fn list_nodes(&self) -> Vec<&str> {
        self.node_names.keys().map(|s| s.as_str()).collect()
    }

    /// Build the pipeline and return a ready-to-run scheduler
    pub fn build(mut self) -> Result<Scheduler, String> {
        info!(
            "Building pipeline with {} nodes and {} connections",
            self.nodes.len(),
            self.connections.len()
        );

        let mut scheduler = Scheduler::new();
        let registry = TYPE_REGISTRY.lock().unwrap();

        type PortKey = (usize, usize);
        let node_by_id: HashMap<usize, &Box<dyn ProcessNode>> =
            self.nodes.iter().map(|(id, node)| (*id, node)).collect();

        // Phase 0: negotiate a connection protocol per pending connection,
        // intersecting the producer's output_protocols (preference order)
        // with the consumer's input_protocols. Every node defaults to
        // `[Stream]` on both ends, so a connection between two nodes that
        // don't know about richer protocols always negotiates `Stream` —
        // today's behavior, unchanged.
        let mut protocols: Vec<ProtocolKind> = Vec::with_capacity(self.connections.len());
        for conn in &self.connections {
            let from_node = node_by_id
                .get(&conn.from_node)
                .ok_or_else(|| format!("Node {} not found", conn.from_node))?;
            let to_node = node_by_id
                .get(&conn.to_node)
                .ok_or_else(|| format!("Node {} not found", conn.to_node))?;
            let produced = from_node.output_protocols(conn.from_port);
            let accepted = to_node.input_protocols(conn.to_port);
            let protocol = produced
                .iter()
                .find(|p| accepted.contains(p))
                .copied()
                .ok_or_else(|| {
                    format!(
                        "No common connection protocol between node {} port {} and node {} port {}",
                        conn.from_node, conn.from_port, conn.to_node, conn.to_port
                    )
                })?;
            protocols.push(protocol);
        }

        // Phase 0.5: build one EdgeQuery handle per producing port that has
        // at least one EdgeQuery-negotiated destination. `input_queries` is
        // `&[]` today — only zero-input source nodes implement `edge_query`
        // — so this doesn't need to run in dependency order; a future
        // pass-through node would upgrade this to a topological pass.
        let mut edge_queries: HashMap<PortKey, Arc<dyn EdgeQuery>> = HashMap::new();
        for (conn, &protocol) in self.connections.iter().zip(&protocols) {
            if protocol != ProtocolKind::EdgeQuery {
                continue;
            }
            let key = (conn.from_node, conn.from_port);
            if edge_queries.contains_key(&key) {
                continue;
            }
            let from_node = node_by_id[&conn.from_node];
            let handle = from_node.edge_query(conn.from_port, &[]).ok_or_else(|| {
                format!(
                    "Node {} port {} negotiated EdgeQuery but declined to provide a handle",
                    conn.from_node, conn.from_port
                )
            })?;
            edge_queries.insert(key, handle);
        }

        // Phase 1: Create channels for Stream-negotiated connections only,
        // accumulating receivers and senders; collect the EdgeQuery handle
        // for each EdgeQuery-negotiated destination input. A connection
        // negotiated as EdgeQuery never gets a channel, so a producer that
        // only has EdgeQuery destinations for a given port sees it as
        // unconnected in Phase 2 below (e.g. a self-threading source's
        // per-destination thread simply doesn't spawn for it).
        let mut receivers: HashMap<PortKey, Box<dyn Any + Send>> = HashMap::new();
        let mut senders: HashMap<PortKey, (TypeId, Vec<Box<dyn Any + Send>>)> = HashMap::new();
        let mut input_edge_queries: HashMap<PortKey, Arc<dyn EdgeQuery>> = HashMap::new();

        for (conn, &protocol) in self.connections.iter().zip(&protocols) {
            match protocol {
                ProtocolKind::Stream => {
                    let (tx, rx) = registry
                        .create_channel(conn.type_id, conn.buffer_size)
                        .ok_or_else(|| format!("Type {:?} not registered. Call register_type::<T>() before building pipeline.", conn.type_id))?;

                    receivers.insert((conn.to_node, conn.to_port), rx);
                    senders
                        .entry((conn.from_node, conn.from_port))
                        .or_insert_with(|| (conn.type_id, Vec::new()))
                        .1
                        .push(tx);
                }
                ProtocolKind::EdgeQuery => {
                    let handle = edge_queries[&(conn.from_node, conn.from_port)].clone();
                    input_edge_queries.insert((conn.to_node, conn.to_port), handle);
                }
            }
        }

        // Phase 2: Start all nodes, wrapping outputs inline
        let watchdog = scheduler.watchdog().clone();

        for (node_id, node) in self.nodes.drain(..) {
            let node_name = node.name().to_string();
            let num_inputs = node.num_inputs();
            let num_outputs = node.num_outputs();
            let input_schemas = node.input_schema();
            let output_schemas = node.output_schema();

            debug!("Starting node {}: {}", node_id, node_name);

            // Collect inputs (unconnected inputs are allowed - nodes may have optional inputs)
            let input_ports: Vec<_> = (0..num_inputs)
                .map(|i| {
                    let port = receivers
                        .remove(&(node_id, i))
                        .map(InputPort::from_type_erased)
                        .unwrap_or_else(|| {
                            // Unconnected input: use dummy port
                            InputPort::from_type_erased(Box::new(()) as Box<dyn Any + Send>)
                        })
                        .with_edge_query(input_edge_queries.remove(&(node_id, i)));

                    // Inject watchdog context
                    let port_name = input_schemas
                        .get(i)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| format!("in{}", i));
                    port.with_watchdog(watchdog.clone(), node_name.clone(), port_name)
                })
                .collect();

            // Collect outputs (unconnected outputs are allowed - nodes must check before sending)
            let output_ports: Result<Vec<_>, String> = (0..num_outputs)
                .map(|i| {
                    let port = if let Some((type_id, sender_list)) = senders.remove(&(node_id, i)) {
                        registry
                            .wrap_output(type_id, sender_list)
                            .map(OutputPort::from_type_erased)?
                    } else {
                        // Unconnected output: use dummy port
                        OutputPort::from_type_erased(Box::new(()) as Box<dyn Any + Send>)
                    };

                    // Inject watchdog context
                    let port_name = output_schemas
                        .get(i)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| format!("out{}", i));
                    Ok(port.with_watchdog(watchdog.clone(), node_name.clone(), port_name))
                })
                .collect();
            let output_ports = output_ports?;

            scheduler.start_process(node, input_ports, output_ports);
        }

        drop(registry);
        info!(
            "Pipeline built successfully with {} threads",
            scheduler.num_threads()
        );
        Ok(scheduler)
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::Sample;
    use crate::runtime::node::ProcessNode;
    use crate::runtime::ports::PortSchema;
    use std::any::TypeId;

    // Minimal test node implementations
    struct TestSource;
    impl ProcessNode for TestSource {
        fn name(&self) -> &str {
            "test_source"
        }
        fn num_inputs(&self) -> usize {
            0
        }
        fn num_outputs(&self) -> usize {
            1
        }
        fn input_schema(&self) -> Vec<PortSchema> {
            vec![]
        }
        fn output_schema(&self) -> Vec<PortSchema> {
            vec![PortSchema {
                name: "out".to_string(),
                type_id: TypeId::of::<Sample>(),
                index: 0,
                direction: crate::runtime::ports::PortDirection::Output,
            }]
        }
        fn work(
            &mut self,
            _inputs: &[crate::runtime::node::InputPort],
            _outputs: &[crate::runtime::node::OutputPort],
        ) -> crate::runtime::errors::WorkResult<usize> {
            Ok(0)
        }
    }

    struct TestSink;
    impl ProcessNode for TestSink {
        fn name(&self) -> &str {
            "test_sink"
        }
        fn num_inputs(&self) -> usize {
            1
        }
        fn num_outputs(&self) -> usize {
            0
        }
        fn input_schema(&self) -> Vec<PortSchema> {
            vec![PortSchema {
                name: "in".to_string(),
                type_id: TypeId::of::<Sample>(),
                index: 0,
                direction: crate::runtime::ports::PortDirection::Input,
            }]
        }
        fn output_schema(&self) -> Vec<PortSchema> {
            vec![]
        }
        fn work(
            &mut self,
            _inputs: &[crate::runtime::node::InputPort],
            _outputs: &[crate::runtime::node::OutputPort],
        ) -> crate::runtime::errors::WorkResult<usize> {
            Ok(0)
        }
    }

    struct TestProcessor;
    impl ProcessNode for TestProcessor {
        fn name(&self) -> &str {
            "test_processor"
        }
        fn num_inputs(&self) -> usize {
            1
        }
        fn num_outputs(&self) -> usize {
            1
        }
        fn input_schema(&self) -> Vec<PortSchema> {
            vec![PortSchema {
                name: "in".to_string(),
                type_id: TypeId::of::<Sample>(),
                index: 0,
                direction: crate::runtime::ports::PortDirection::Input,
            }]
        }
        fn output_schema(&self) -> Vec<PortSchema> {
            vec![PortSchema {
                name: "out".to_string(),
                type_id: TypeId::of::<Sample>(),
                index: 0,
                direction: crate::runtime::ports::PortDirection::Output,
            }]
        }
        fn work(
            &mut self,
            _inputs: &[crate::runtime::node::InputPort],
            _outputs: &[crate::runtime::node::OutputPort],
        ) -> crate::runtime::errors::WorkResult<usize> {
            Ok(0)
        }
    }

    #[test]
    fn test_single_connection() {
        let mut pipeline = Pipeline::new();
        pipeline.add_process("source", TestSource).unwrap();
        pipeline.add_process("sink", TestSink).unwrap();

        let result = pipeline.connect("source", "out", "sink", "in");
        assert!(result.is_ok(), "Single connection should succeed");
    }

    #[test]
    fn test_duplicate_input_connection_rejected() {
        let mut pipeline = Pipeline::new();
        pipeline.add_process("source1", TestSource).unwrap();
        pipeline.add_process("source2", TestSource).unwrap();
        pipeline.add_process("sink", TestSink).unwrap();

        // First connection should succeed
        pipeline.connect("source1", "out", "sink", "in").unwrap();

        // Second connection to same input should fail
        let result = pipeline.connect("source2", "out", "sink", "in");
        assert!(
            result.is_err(),
            "Duplicate input connection should be rejected"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("already connected")
        );
    }

    #[test]
    fn test_multiple_output_connections_allowed() {
        let mut pipeline = Pipeline::new();
        pipeline.add_process("source", TestSource).unwrap();
        pipeline.add_process("sink1", TestSink).unwrap();
        pipeline.add_process("sink2", TestSink).unwrap();

        // Multiple connections from same output should succeed (broadcasting)
        let result1 = pipeline.connect("source", "out", "sink1", "in");
        let result2 = pipeline.connect("source", "out", "sink2", "in");

        assert!(result1.is_ok(), "First output connection should succeed");
        assert!(
            result2.is_ok(),
            "Second output connection should succeed (broadcasting)"
        );
    }

    #[test]
    fn test_connection_to_nonexistent_node() {
        let mut pipeline = Pipeline::new();
        pipeline.add_process("source", TestSource).unwrap();

        let result = pipeline.connect("source", "out", "nonexistent", "in");
        assert!(
            result.is_err(),
            "Connection to nonexistent node should fail"
        );
    }

    #[test]
    fn test_connection_to_nonexistent_port() {
        let mut pipeline = Pipeline::new();
        pipeline.add_process("source", TestSource).unwrap();
        pipeline.add_process("sink", TestSink).unwrap();

        let result = pipeline.connect("source", "wrong_port", "sink", "in");
        assert!(
            result.is_err(),
            "Connection to nonexistent port should fail"
        );
    }

    #[test]
    fn test_chain_connections() {
        let mut pipeline = Pipeline::new();
        pipeline.add_process("source", TestSource).unwrap();
        pipeline.add_process("processor", TestProcessor).unwrap();
        pipeline.add_process("sink", TestSink).unwrap();

        let result1 = pipeline.connect("source", "out", "processor", "in");
        let result2 = pipeline.connect("processor", "out", "sink", "in");

        assert!(result1.is_ok(), "First chain connection should succeed");
        assert!(result2.is_ok(), "Second chain connection should succeed");
    }

    #[test]
    fn test_custom_buffer_size() {
        let mut pipeline = Pipeline::new();
        pipeline.add_process("source", TestSource).unwrap();
        pipeline.add_process("sink", TestSink).unwrap();

        let result = pipeline.connect_with_buffer("source", "out", "sink", "in", 10000);
        assert!(
            result.is_ok(),
            "Connection with custom buffer size should succeed"
        );
    }

    #[test]
    fn test_duplicate_node_name_rejected() {
        let mut pipeline = Pipeline::new();
        let result1 = pipeline.add_process("node1", TestSource);
        let result2 = pipeline.add_process("node1", TestSource);

        assert!(result1.is_ok(), "First node addition should succeed");
        assert!(result2.is_err(), "Duplicate node name should be rejected");
        assert!(result2.unwrap_err().contains("already exists"));
    }

    #[test]
    fn test_list_nodes() {
        let mut pipeline = Pipeline::new();
        pipeline.add_process("source", TestSource).unwrap();
        pipeline.add_process("sink", TestSink).unwrap();

        let nodes = pipeline.list_nodes();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.contains(&"source"));
        assert!(nodes.contains(&"sink"));
    }
}
