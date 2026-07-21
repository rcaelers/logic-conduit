//! Native persistent-cache capability boundary.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use logic_analyzer_processing::nodes::sources::DslFileCaptureDataSource;
use node_graph::{GraphState, NodeId};
use signal_processing::{CaptureDataSource, IndexedAnnotationStore, PersistentStoreConfig, Word};

use super::errors::CompileError;
use super::graph::{BuilderRegistry, CompiledEdge, CompiledGraph, RuntimeBuilder, compiled_node};
use super::port_kind::PortKind;

const DERIVED_CACHE_ABI_VERSION: u32 = 2;

pub(crate) fn assign_viewer_caches(compiled: &mut CompiledGraph) {
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
                *slot = Some(PersistentStoreConfig::new(PathBuf::new(), key));
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

pub(crate) fn configure_directory(compiled: &mut CompiledGraph, directory: Option<&Path>) {
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

pub(crate) fn prepare_execution(
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
    let _ = signal_processing::cleanup_cache(&first.directory, first.max_cache_bytes, &pinned);
}

pub(crate) fn cache_configs_by_node(
    graph: &GraphState,
    registry: &BuilderRegistry,
    directory: &Path,
) -> Result<HashMap<NodeId, Vec<PersistentStoreConfig>>, Vec<CompileError>> {
    let mut compiled = super::graph::lower(graph, registry)?;
    configure_directory(&mut compiled, Some(directory));
    let mut result: HashMap<NodeId, Vec<PersistentStoreConfig>> = HashMap::new();
    for viewer in compiled
        .nodes
        .iter()
        .filter(|node| node.builder == "Viewer")
    {
        for (member, config) in viewer.viewer_word_caches.iter().enumerate() {
            let Some(config) = config else {
                continue;
            };
            let input_name = format!("in{member}");
            let Some(edge) = compiled.edges.iter().find(|edge| {
                edge.to.0 == viewer.id
                    && edge.to.1 == input_name
                    && edge.kind == PortKind::of::<Word>()
            }) else {
                continue;
            };

            let mut stack = vec![viewer.id, edge.from.0];
            let mut visited = HashSet::new();
            while let Some(node_id) = stack.pop() {
                if !visited.insert(node_id) {
                    continue;
                }
                let configs = result.entry(node_id).or_default();
                if !configs
                    .iter()
                    .any(|existing| existing.cache_key == config.cache_key)
                {
                    configs.push(config.clone());
                }
                stack.extend(
                    compiled
                        .edges
                        .iter()
                        .filter(|incoming| incoming.to.0 == node_id)
                        .map(|incoming| incoming.from.0),
                );
            }
        }
    }
    Ok(result)
}

pub(crate) fn persistent_lane_key(
    compiled: &CompiledGraph,
    viewer_id: NodeId,
    member: usize,
    edge: &CompiledEdge,
) -> Option<[u8; 32]> {
    let mut memo = HashMap::new();
    let upstream = persistent_upstream_key(compiled, edge.from.0, &mut memo)?;
    let viewer = compiled_node(compiled, viewer_id);
    let mut hasher = blake3::Hasher::new();
    hash_field(&mut hasher, b"dsl-derived-lane-cache-v1");
    hash_field(&mut hasher, env!("CARGO_PKG_VERSION").as_bytes());
    hash_field(&mut hasher, &DERIVED_CACHE_ABI_VERSION.to_le_bytes());
    hash_field(&mut hasher, &canonical_json_bytes(&viewer.state));
    hash_field(&mut hasher, &(member as u64).to_le_bytes());
    hash_field(&mut hasher, edge.from.1.as_bytes());
    hash_field(&mut hasher, edge.kind.name().as_bytes());
    hash_field(&mut hasher, &upstream);
    Some(*hasher.finalize().as_bytes())
}

fn persistent_upstream_key(
    compiled: &CompiledGraph,
    node_id: NodeId,
    memo: &mut HashMap<NodeId, [u8; 32]>,
) -> Option<[u8; 32]> {
    if let Some(key) = memo.get(&node_id) {
        return Some(*key);
    }
    let node = compiled_node(compiled, node_id);
    let mut hasher = blake3::Hasher::new();
    hash_field(&mut hasher, b"node");
    hash_field(&mut hasher, node.builder.as_bytes());
    hash_field(&mut hasher, &canonical_json_bytes(&node.state));
    if node.builder == "DSL File Source" {
        if compiled.edges.iter().any(|edge| edge.to.0 == node_id) {
            return None;
        }
        hash_capture_source(&mut hasher, &node.state)?;
    }
    let mut incoming: Vec<_> = compiled
        .edges
        .iter()
        .filter(|edge| edge.to.0 == node_id)
        .collect();
    incoming.sort_by(|left, right| {
        (&left.to.1, &left.from.1, left.kind.name()).cmp(&(
            &right.to.1,
            &right.from.1,
            right.kind.name(),
        ))
    });
    for edge in incoming {
        hash_field(&mut hasher, edge.to.1.as_bytes());
        hash_field(&mut hasher, edge.from.1.as_bytes());
        hash_field(&mut hasher, edge.kind.name().as_bytes());
        hash_field(
            &mut hasher,
            &persistent_upstream_key(compiled, edge.from.0, memo)?,
        );
    }
    let key = *hasher.finalize().as_bytes();
    memo.insert(node_id, key);
    Some(key)
}

fn hash_capture_source(hasher: &mut blake3::Hasher, state: &Value) -> Option<()> {
    let state: crate::nodes::DslFileSourceState = serde_json::from_value(state.clone()).ok()?;
    let path = std::fs::canonicalize(&state.file.value).ok()?;
    hash_capture_file_identity(hasher, &path)?;
    let source = DslFileCaptureDataSource::open(&path).ok()?;
    let metadata = source.metadata();
    hash_field(hasher, &metadata.samplerate_hz.to_bits().to_le_bytes());
    hash_field(hasher, &metadata.total_samples.to_le_bytes());
    hash_field(hasher, &(metadata.total_probes as u64).to_le_bytes());
    for name in &metadata.probe_names {
        hash_field(hasher, name.as_bytes());
    }
    Some(())
}

pub(crate) fn hash_capture_file_identity(hasher: &mut blake3::Hasher, path: &Path) -> Option<()> {
    let file_metadata = std::fs::metadata(path).ok()?;
    let modified_ns = file_metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    hash_field(hasher, path.to_string_lossy().as_bytes());
    hash_field(hasher, &file_metadata.len().to_le_bytes());
    hash_field(hasher, &modified_ns.to_le_bytes());
    Some(())
}

fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    fn append(value: &Value, output: &mut Vec<u8>) {
        match value {
            Value::Null => output.push(b'n'),
            Value::Bool(value) => output.extend_from_slice(if *value { b"t" } else { b"f" }),
            Value::Number(value) => {
                output.push(b'#');
                append_bytes(output, value.to_string().as_bytes());
            }
            Value::String(value) => {
                output.push(b'"');
                append_bytes(output, value.as_bytes());
            }
            Value::Array(values) => {
                output.push(b'[');
                for value in values {
                    append(value, output);
                }
                output.push(b']');
            }
            Value::Object(values) => {
                output.push(b'{');
                let mut fields: Vec<_> = values.iter().collect();
                fields.sort_by_key(|(name, _)| *name);
                for (name, value) in fields {
                    append_bytes(output, name.as_bytes());
                    append(value, output);
                }
                output.push(b'}');
            }
        }
    }
    fn append_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
        output.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        output.extend_from_slice(bytes);
    }
    let mut output = Vec::new();
    append(value, &mut output);
    output
}

fn hash_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}
