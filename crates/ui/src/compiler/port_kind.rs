//! Open payload-type identity for compiled pipeline edges.
//!
//! [`PortKind`] is how a UI socket maps onto a concrete runtime channel
//! payload — the compiler-layer analogue of `node_graph::SocketDef` (graph
//! layer) and `dsl::runtime::register_type::<T>()` (runtime layer). Both of
//! those are open (implement a trait / call a generic fn, no enum to edit);
//! this module brings `PortKind` in line with them so a new payload type —
//! including one defined by an out-of-tree plugin crate — never requires
//! editing this file or any `RuntimeBuilder` it doesn't own.
//!
//! Implement [`PortValue`] once per Rust type that flows through a compiled
//! edge, then use `PortKind::of::<T>()` wherever the old code constructed a
//! fixed enum variant.

use std::any::TypeId;
use std::fmt;

/// A stream payload type a compiled port can carry. One `impl` per Rust
/// type — no central registry, mirroring `node_graph::SocketDef`.
pub trait PortValue: Send + Sync + Clone + 'static {
    /// Stable identity used in diagnostics (`PortKind`'s `Debug` output).
    fn kind_name() -> &'static str;

    /// Channel buffer size (§5.3). `producer_is_source` is the compiled
    /// edge's "producer is a graph source" fact, not a property of `Self` —
    /// only `Sample` cares. Default (100) matches every other existing
    /// kind's fixed size.
    fn buffer_size(_producer_is_source: bool) -> usize {
        100
    }
}

/// How a UI socket maps onto a runtime channel payload. Identity is
/// `TypeId::of::<T>()`, captured via [`PortKind::of`] — the open
/// replacement for what used to be a fixed enum.
///
/// `PartialEq`/`Eq`/`Hash` are hand-written over `type_id` alone —
/// `buffer_size_fn` is not `#[derive]`d over, since comparing `fn` pointers
/// isn't guaranteed stable across codegen units (rustc warns on this); two
/// `PortKind::of::<Sample>()` calls always carry the same `type_id`
/// regardless, so identity doesn't need the fn pointer at all.
#[derive(Clone, Copy)]
pub struct PortKind {
    type_id: TypeId,
    name: &'static str,
    buffer_size_fn: fn(bool) -> usize,
}

impl PartialEq for PortKind {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id
    }
}
impl Eq for PortKind {}
impl std::hash::Hash for PortKind {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.type_id.hash(state);
    }
}

impl PortKind {
    pub fn of<T: PortValue>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            name: T::kind_name(),
            buffer_size_fn: T::buffer_size,
        }
    }

    pub fn type_id(&self) -> TypeId {
        self.type_id
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn buffer_size(&self, producer_is_source: bool) -> usize {
        (self.buffer_size_fn)(producer_is_source)
    }
}

impl fmt::Debug for PortKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

// ── Built-in payload types ──────────────────────────────────────────────────

use dsl::{NumberSample, Sample, SampleBlock, TextSample, Trigger, Word};

impl PortValue for Sample {
    fn kind_name() -> &'static str {
        "SampleEdge"
    }
    fn buffer_size(producer_is_source: bool) -> usize {
        if producer_is_source { 10_000_000 } else { 1_000 }
    }
}

impl PortValue for SampleBlock {
    fn kind_name() -> &'static str {
        "Block"
    }
    fn buffer_size(_producer_is_source: bool) -> usize {
        4
    }
}

impl PortValue for Word {
    fn kind_name() -> &'static str {
        "Word"
    }
    // Uses the trait default (100) — see §5.3 in
    // `ANALYSIS_PIPELINE_DESIGN.md`: word-shaped kinds no longer get a
    // special-cased large buffer to silently absorb skew between branches;
    // a graph that genuinely needs that inserts an explicit `Buffer` node.
}

impl PortValue for Trigger {
    fn kind_name() -> &'static str {
        "Trigger"
    }
}

impl PortValue for NumberSample {
    fn kind_name() -> &'static str {
        "Number"
    }
}

impl PortValue for TextSample {
    fn kind_name() -> &'static str {
        "Text"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_type_is_equal() {
        assert_eq!(PortKind::of::<Sample>(), PortKind::of::<Sample>());
    }

    #[test]
    fn different_types_are_not_equal() {
        assert_ne!(PortKind::of::<Sample>(), PortKind::of::<SampleBlock>());
    }

    #[test]
    fn debug_matches_old_enum_variant_names() {
        assert_eq!(format!("{:?}", PortKind::of::<Sample>()), "SampleEdge");
        assert_eq!(format!("{:?}", PortKind::of::<SampleBlock>()), "Block");
    }
}
