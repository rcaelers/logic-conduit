use std::collections::HashMap;

use node_graph::NodeTypeRegistry;

use crate::RuntimeBuilder;

pub(crate) fn register_nodes(_registry: &mut NodeTypeRegistry) {}

pub(crate) fn register_builders(_builders: &mut HashMap<String, Box<dyn RuntimeBuilder>>) {}
