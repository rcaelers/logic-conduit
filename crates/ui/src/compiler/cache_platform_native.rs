//! Native persistent-cache capability boundary.

use std::path::Path;

use super::{BuilderRegistry, CompiledGraph};

pub(super) fn assign_viewer_caches(compiled: &mut CompiledGraph) {
    super::assign_persistent_viewer_caches(compiled);
}

pub(super) fn configure_directory(compiled: &mut CompiledGraph, directory: Option<&Path>) {
    super::configure_persistent_cache_directory(compiled, directory);
}

pub(super) fn prepare_execution(
    compiled: &CompiledGraph,
    registry: &BuilderRegistry,
) -> (CompiledGraph, bool) {
    super::prepare_persistent_cache(compiled);
    super::persistent_execution_graph(compiled, registry)
}
