//! Single entry point for a compile-time plugin crate's `register()` fn.
//!
//! Without this, a plugin has to juggle three differently-shaped APIs by
//! hand: `NodeTypeRegistry::register::<T>()` (generic method),
//! `BuilderRegistry::insert(name, Box<dyn RuntimeBuilder>)` (named
//! trait-object insert), and the free function
//! `dsl::runtime::register_type::<T>()`. [`PluginContext`] wraps all three
//! behind one object and one consistent, chainable `register_*` naming
//! scheme.
//!
//! `SocketDef` and `PortValue` types need no entry here at all — both are
//! pure compile-time identity, backed by no registry to call into:
//!
//! - A `SocketDef` (e.g. a plugin's own socket type) just needs `impl
//!   SocketDef for T`, then `InputDef::new::<T>()`/`OutputDef::new::<T>()`
//!   read `T::type_name()`/`color()`/`shape()` straight off the type. The
//!   only place socket identity is ever cached (`NodeTypeRegistry`'s
//!   re-skinning table) is populated as a side effect of
//!   [`Self::register_node`] walking that node's `inputs()`/`outputs()` —
//!   there is deliberately no separate socket registry (see
//!   `docs/NODE_GRAPH_DESIGN.md`'s no-global-cast-table stance).
//! - A `PortValue` (e.g. a plugin's own stream payload marker) just needs
//!   `impl PortValue for T`; `PortKind::of::<T>()` computes identity fresh
//!   from `TypeId::of::<T>()` every call, nothing to precompute or cache.
//!   [`Self::register_payload`] is a *different* concern: it registers `T`
//!   with `dsl::runtime`'s channel machinery (how to create/wrap a
//!   `crossbeam` channel of `T`) — that one's a real, TypeId-keyed runtime
//!   registry and does need the call, `T: PortValue` is just a convenient
//!   bound (identical to what the runtime registry itself requires).

use super::port_kind::PortValue;
use super::{BuilderRegistry, RuntimeBuilder};
use node_graph::{NodeDef, NodeTypeRegistry};

/// Everything a plugin's `register(ctx: &mut PluginContext)` needs, in one
/// place.
pub struct PluginContext<'a> {
    nodes: &'a mut NodeTypeRegistry,
    builders: &'a mut BuilderRegistry,
}

impl<'a> PluginContext<'a> {
    pub fn new(nodes: &'a mut NodeTypeRegistry, builders: &'a mut BuilderRegistry) -> Self {
        Self { nodes, builders }
    }

    /// Registers a graph node type. Socket types it references via
    /// `inputs()`/`outputs()` need no separate call (see module doc).
    pub fn register_node<T: NodeDef>(&mut self) -> &mut Self {
        self.nodes.register::<T>();
        self
    }

    /// Registers the runtime builder for a node type, keyed by `name` —
    /// must match the corresponding `NodeDef::name()`.
    pub fn register_builder(
        &mut self,
        name: impl Into<String>,
        builder: Box<dyn RuntimeBuilder>,
    ) -> &mut Self {
        self.builders.insert(name, builder);
        self
    }

    /// Registers `T` with the runtime channel machinery so pipelines can
    /// actually move it through a connection. Does *not* register the
    /// `PortValue` impl itself — that needs no registration at all (see
    /// module doc); `T: PortValue` here is just the same bound the runtime
    /// registry already requires.
    pub fn register_payload<T: PortValue>(&mut self) -> &mut Self {
        dsl::runtime::register_type::<T>();
        self
    }
}
