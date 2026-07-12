//! Native persistent-cache capability boundary.

use std::collections::HashSet;
use std::path::Path;

use dsl::{IndexedAnnotationStore, PersistentStoreConfig, Word};

use super::{
    BuilderRegistry, CompiledGraph, PortKind, RuntimeBuilder, compiled_node, persistent_lane_key,
};

pub(super) fn assign_viewer_caches(compiled: &mut CompiledGraph) {
    let viewer_ids: Vec<_> = compiled
        .nodes
        .iter()
        .filter(|node| node.builder == "Viewer")
        .map(|node| node.id)
        .collect();
    let mut assignments = Vec::new();
    for viewer_id in viewer_ids {
        let member_count = compiled_node(compiled, viewer_id).resolved.member_count(0);
        let mut caches = vec![None; member_count];
        for (member, slot) in caches.iter_mut().enumerate() {
            let input_name = format!("in{member}");
            let Some(edge) = compiled.edges.iter().find(|edge| {
                edge.to.0 == viewer_id
                    && edge.to.1 == input_name
                    && edge.kind == PortKind::of::<Word>()
            }) else {
                continue;
            };
            if let Some(key) = persistent_lane_key(compiled, viewer_id, member, edge) {
                *slot = Some(PersistentStoreConfig::new(
                    dsl::default_cache_directory(),
                    key,
                ));
            }
        }
        assignments.push((viewer_id, caches));
    }
    for (viewer_id, caches) in assignments {
        let node = compiled
            .nodes
            .iter_mut()
            .find(|node| node.id == viewer_id)
            .expect("viewer node exists");
        node.viewer_word_caches = caches;
    }
}

pub(super) fn configure_directory(compiled: &mut CompiledGraph, directory: Option<&Path>) {
    for node in &mut compiled.nodes {
        for slot in &mut node.viewer_word_caches {
            match (slot.as_mut(), directory) {
                (_, None) => *slot = None,
                (Some(config), Some(directory)) => config.directory = directory.to_path_buf(),
                (None, Some(_)) => {}
            }
        }
    }
}

pub(super) fn prepare_execution(
    compiled: &CompiledGraph,
    registry: &BuilderRegistry,
) -> (CompiledGraph, bool) {
    prepare_cache(compiled);

    let mut execution = compiled.clone();
    let mut cached_inputs = HashSet::new();
    for viewer in &compiled.nodes {
        if viewer.builder != "Viewer" {
            continue;
        }
        for (member, config) in viewer.viewer_word_caches.iter().enumerate() {
            let Some(config) = config else {
                continue;
            };
            if IndexedAnnotationStore::open_persistent(config)
                .ok()
                .flatten()
                .is_some()
            {
                cached_inputs.insert((viewer.id, format!("in{member}")));
            }
        }
    }
    if cached_inputs.is_empty() {
        return (execution, false);
    }
    execution
        .edges
        .retain(|edge| !cached_inputs.contains(&(edge.to.0, edge.to.1.clone())));

    let mut reachable: HashSet<_> = execution
        .nodes
        .iter()
        .filter(|node| {
            registry
                .get(&node.builder)
                .is_some_and(RuntimeBuilder::is_sink)
        })
        .map(|node| node.id)
        .collect();
    let mut stack: Vec<_> = reachable.iter().copied().collect();
    while let Some(node_id) = stack.pop() {
        for edge in execution.edges.iter().filter(|edge| edge.to.0 == node_id) {
            if reachable.insert(edge.from.0) {
                stack.push(edge.from.0);
            }
        }
    }
    execution.nodes.retain(|node| reachable.contains(&node.id));
    execution
        .edges
        .retain(|edge| reachable.contains(&edge.from.0) && reachable.contains(&edge.to.0));
    (execution, true)
}

fn prepare_cache(compiled: &CompiledGraph) {
    let configs: Vec<_> = compiled
        .nodes
        .iter()
        .flat_map(|node| node.viewer_word_caches.iter().flatten())
        .collect();
    let Some(first) = configs.first() else {
        return;
    };
    let pinned: Vec<_> = configs.iter().map(|config| config.cache_key).collect();
    let _ = dsl::cleanup_cache(&first.directory, first.max_cache_bytes, &pinned);
}
