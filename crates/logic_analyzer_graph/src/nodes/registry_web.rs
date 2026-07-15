use std::collections::HashMap;

use node_graph::NodeTypeRegistry;

use crate::compiler::RuntimeBuilder;

pub(super) fn register_nodes(_registry: &mut NodeTypeRegistry) {}

pub(super) fn register_builders(_builders: &mut HashMap<String, Box<dyn RuntimeBuilder>>) {}
