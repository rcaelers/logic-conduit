//! Single entry point for a compile-time plugin crate's `register()` fn.
//!
//! Without this, a plugin has to juggle three differently-shaped APIs by
//! hand: `NodeTypeRegistry::register::<T>()` (generic method),
//! `BuilderRegistry::insert(name, Box<dyn RuntimeBuilder>)` (named
//! trait-object insert), and the free function
//! `signal_processing::register_type::<T>()`. [`PluginContext`] wraps all three
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
//!   with `signal_processing`'s channel machinery (how to create/wrap a
//!   `crossbeam` channel of `T`) — that one's a real, TypeId-keyed runtime
//!   registry and does need the call, `T: PortValue` is just a convenient
//!   bound (identical to what the runtime registry itself requires).
//!
//! A payload that should be retained for a waveform, table, or plugin panel
//! additionally uses [`Self::register_collected_payload`]. That creates the
//! runtime channel registration and assigns the durable identity used by
//! future collection adapters and serialized presentation state. It does not
//! make every `PortValue` collectable.

use node_graph::{NodeDef, NodeTypeRegistry};

use super::graph::{BuilderRegistry, RuntimeBuilder};
use super::port_kind::PortValue;

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
        signal_processing::register_type::<T>();
        self
    }

    /// Registers a payload with the runtime channel factory and assigns its
    /// durable collected-payload identity. The payload's later adapter
    /// registration defines ingestion, storage, and presentation semantics.
    pub fn register_collected_payload<T: PortValue>(
        &mut self,
        stable_id: impl Into<String>,
    ) -> Result<&mut Self, signal_processing::CollectedPayloadRegistrationError> {
        self.builders.register_collected_payload::<T>(stable_id)?;
        Ok(self)
    }

    /// Registers a payload's typed collector adapter. The adapter owns its
    /// input draining and retained query state; generic runtime code only
    /// schedules it and publishes the opaque query handle.
    pub fn register_collected_payload_adapter<T: PortValue>(
        &mut self,
        stable_id: impl Into<String>,
        adapter: std::sync::Arc<dyn signal_processing::CollectedPayloadAdapter>,
    ) -> Result<&mut Self, signal_processing::CollectedPayloadRegistrationError> {
        self.builders
            .register_collected_payload_adapter::<T>(stable_id, adapter)?;
        Ok(self)
    }

    /// Registers a collected payload adapter and opts its payload into
    /// generic data subscriptions such as the Viewer. The payload owner must
    /// supply any presentation metadata required by its own renderer.
    pub fn register_viewable_collected_payload_adapter<T: PortValue>(
        &mut self,
        stable_id: impl Into<String>,
        adapter: std::sync::Arc<dyn signal_processing::CollectedPayloadAdapter>,
        presentation: crate::DefaultViewerPayloadPresentation,
    ) -> Result<&mut Self, signal_processing::CollectedPayloadRegistrationError> {
        self.builders
            .register_viewable_collected_payload_adapter::<T>(stable_id, adapter, presentation)?;
        Ok(self)
    }
}

#[cfg(test)]
mod plugin_tests {
    use node_graph::NodeTypeRegistry;

    use super::*;

    #[derive(Clone)]
    struct PluginEvent;

    impl PortValue for PluginEvent {
        fn kind_name() -> &'static str {
            "Plugin Event"
        }
    }

    #[test]
    fn compile_time_plugin_registers_a_collected_payload_identity() {
        let mut nodes = NodeTypeRegistry::new();
        let mut builders = BuilderRegistry::standard();

        PluginContext::new(&mut nodes, &mut builders)
            .register_collected_payload::<PluginEvent>("org.example.plugin-event/v1")
            .unwrap();

        assert_eq!(
            builders
                .collected_payloads()
                .descriptor::<PluginEvent>()
                .unwrap()
                .stable_id(),
            "org.example.plugin-event/v1"
        );
    }
}
