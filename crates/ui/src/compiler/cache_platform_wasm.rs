//! Wasm persistent-cache capability boundary.
//!
//! The derived-word store remains available through its in-memory backend;
//! only filesystem persistence and cache-pruning are absent.

use std::collections::HashMap;
use std::path::Path;

use dsl::PersistentStoreConfig;
use node_graph::{GraphState, NodeId};

use super::{BuilderRegistry, CompileError, CompiledGraph};

pub(super) fn assign_viewer_caches(_compiled: &mut CompiledGraph) {}

pub(super) fn configure_directory(_compiled: &mut CompiledGraph, _directory: Option<&Path>) {}

pub(super) fn prepare_execution(
    compiled: &CompiledGraph,
    _registry: &BuilderRegistry,
) -> (CompiledGraph, bool) {
    (compiled.clone(), false)
}

pub(super) fn cache_configs_by_node(
    _graph: &GraphState,
    _registry: &BuilderRegistry,
) -> Result<HashMap<NodeId, Vec<PersistentStoreConfig>>, Vec<CompileError>> {
    Ok(HashMap::new())
}
