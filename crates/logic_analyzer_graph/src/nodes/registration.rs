//! Inventory contract for one atomic graph-node feature.

use std::collections::HashSet;

use node_graph::{NodeDef, NodeTypeRegistry};

use crate::RuntimeBuilder;

/// One independently discoverable graph node paired with its runtime builder.
pub struct GraphNodeRegistration {
    stable_id: &'static str,
    node_name: fn() -> &'static str,
    register_node: fn(&mut NodeTypeRegistry),
    create_builder: fn() -> Box<dyn RuntimeBuilder>,
    required_payloads: &'static [&'static str],
    runtime_setup: &'static [fn()],
}

impl GraphNodeRegistration {
    /// Declares a runnable graph node. The processing implementation remains
    /// private behind `B`; only its graph-owned builder is discoverable.
    pub const fn runnable<N, B>(stable_id: &'static str) -> Self
    where
        N: NodeDef,
        B: RuntimeBuilder + Default + 'static,
    {
        Self {
            stable_id,
            node_name: node_name::<N>,
            register_node: register_node::<N>,
            create_builder: create_builder::<B>,
            required_payloads: &[],
            runtime_setup: &[],
        }
    }

    pub const fn requiring_payloads(mut self, required_payloads: &'static [&'static str]) -> Self {
        self.required_payloads = required_payloads;
        self
    }

    /// Adds idempotent runtime setup owned by this node, such as registering
    /// a custom non-collected channel payload.
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

    pub(crate) fn apply_runtime_setup(&self) {
        for setup in self.runtime_setup {
            setup();
        }
    }

    pub(crate) fn apply_node(&self, registry: &mut NodeTypeRegistry) {
        (self.register_node)(registry);
    }

    pub(crate) fn builder(&self) -> Box<dyn RuntimeBuilder> {
        (self.create_builder)()
    }
}

fn node_name<N: NodeDef>() -> &'static str {
    N::name()
}

fn register_node<N: NodeDef>(registry: &mut NodeTypeRegistry) {
    registry.register::<N>();
}

fn create_builder<B>() -> Box<dyn RuntimeBuilder>
where
    B: RuntimeBuilder + Default + 'static,
{
    Box::<B>::default()
}

inventory::collect!(GraphNodeRegistration);

pub(crate) fn graph_node_registrations() -> Vec<&'static GraphNodeRegistration> {
    let mut registrations = inventory::iter::<GraphNodeRegistration>
        .into_iter()
        .collect::<Vec<_>>();
    validate_graph_node_registrations(&mut registrations);
    registrations
}

fn validate_graph_node_registrations(registrations: &mut Vec<&GraphNodeRegistration>) {
    registrations.sort_by_key(|registration| registration.stable_id());

    let mut stable_ids = HashSet::new();
    let mut names = HashSet::new();
    for registration in registrations {
        assert!(
            !registration.stable_id().trim().is_empty(),
            "graph-node inventory contains an empty stable ID"
        );
        assert!(
            stable_ids.insert(registration.stable_id()),
            "duplicate graph-node inventory stable ID '{}'",
            registration.stable_id()
        );
        assert!(
            names.insert(registration.name()),
            "duplicate graph-node inventory name '{}'",
            registration.name()
        );
    }
}

pub(crate) fn validate_graph_node_payload_requirements(
    payloads: &signal_processing::CollectedPayloadRegistry,
) {
    validate_graph_node_payload_requirements_for(&graph_node_registrations(), payloads);
}

fn validate_graph_node_payload_requirements_for(
    registrations: &[&GraphNodeRegistration],
    payloads: &signal_processing::CollectedPayloadRegistry,
) {
    for registration in registrations {
        for stable_id in registration.required_payloads() {
            assert!(
                payloads.descriptor_by_stable_id(stable_id).is_some(),
                "graph-node inventory feature '{}' requires unavailable collected payload '{}'",
                registration.stable_id(),
                stable_id
            );
        }
    }
}

#[cfg(test)]
mod registration_tests {
    use super::*;

    fn unused_register_node(_registry: &mut NodeTypeRegistry) {}

    fn unused_builder() -> Box<dyn RuntimeBuilder> {
        panic!("test registration builder must not be constructed")
    }

    fn missing_payload_node_name() -> &'static str {
        "Missing Payload Test Node"
    }

    static MISSING_PAYLOAD_REGISTRATION: GraphNodeRegistration = GraphNodeRegistration {
        stable_id: "org.logicconduit.test.missing-payload/v1",
        node_name: missing_payload_node_name,
        register_node: unused_register_node,
        create_builder: unused_builder,
        required_payloads: &["org.logicconduit.test.absent-payload/v1"],
        runtime_setup: &[],
    };

    #[test]
    fn collected_registrations_are_stably_ordered_and_unique() {
        let registrations = graph_node_registrations();
        assert!(registrations.windows(2).all(|pair| {
            pair[0].stable_id() < pair[1].stable_id() && pair[0].name() != pair[1].name()
        }));
    }

    #[test]
    fn duplicate_graph_node_registration_is_rejected() {
        let registration = graph_node_registrations()[0];
        let mut registrations = vec![registration, registration];

        assert!(
            std::panic::catch_unwind(move || {
                validate_graph_node_registrations(&mut registrations)
            })
            .is_err()
        );
    }

    #[test]
    fn missing_collected_payload_dependency_is_rejected() {
        let payloads = signal_processing::CollectedPayloadRegistry::new();

        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                validate_graph_node_payload_requirements_for(
                    &[&MISSING_PAYLOAD_REGISTRATION],
                    &payloads,
                )
            }))
            .is_err()
        );
    }
}
