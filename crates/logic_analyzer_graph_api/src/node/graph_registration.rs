use node_graph::{NodeDef, NodeTypeRegistry};

use super::contracts::RuntimeBuilder;

pub struct GraphNodeRegistration {
    stable_id: &'static str,
    node_name: fn() -> &'static str,
    register_node: fn(&mut NodeTypeRegistry),
    create_builder: Option<fn() -> Box<dyn RuntimeBuilder>>,
    required_payloads: &'static [&'static str],
    runtime_setup: &'static [fn()],
}

impl GraphNodeRegistration {
    pub const fn runnable<N, B>(stable_id: &'static str) -> Self
    where
        N: NodeDef,
        B: RuntimeBuilder + Default + 'static,
    {
        Self {
            stable_id,
            node_name: node_name::<N>,
            register_node: register_node::<N>,
            create_builder: Some(create_builder::<B>),
            required_payloads: &[],
            runtime_setup: &[],
        }
    }

    pub const fn definition<N: NodeDef>(stable_id: &'static str) -> Self {
        Self {
            stable_id,
            node_name: node_name::<N>,
            register_node: register_node::<N>,
            create_builder: None,
            required_payloads: &[],
            runtime_setup: &[],
        }
    }

    pub const fn requiring_payloads(mut self, required_payloads: &'static [&'static str]) -> Self {
        self.required_payloads = required_payloads;
        self
    }

    pub const fn with_runtime_setup(mut self, runtime_setup: &'static [fn()]) -> Self {
        self.runtime_setup = runtime_setup;
        self
    }

    pub const fn stable_id(&self) -> &'static str {
        self.stable_id
    }

    pub fn name(&self) -> &'static str {
        (self.node_name)()
    }

    pub const fn required_payloads(&self) -> &'static [&'static str] {
        self.required_payloads
    }

    #[doc(hidden)]
    pub fn apply_runtime_setup(&self) {
        for setup in self.runtime_setup {
            setup();
        }
    }

    #[doc(hidden)]
    pub fn apply_node(&self, registry: &mut NodeTypeRegistry) {
        (self.register_node)(registry);
    }

    #[doc(hidden)]
    pub fn builder(&self) -> Option<Box<dyn RuntimeBuilder>> {
        self.create_builder.map(|create_builder| create_builder())
    }
}

fn node_name<N: NodeDef>() -> &'static str {
    N::name()
}

fn register_node<N: NodeDef>(registry: &mut NodeTypeRegistry) {
    registry.register::<N>();
}

fn create_builder<B: RuntimeBuilder + Default + 'static>() -> Box<dyn RuntimeBuilder> {
    Box::<B>::default()
}

inventory::collect!(GraphNodeRegistration);
