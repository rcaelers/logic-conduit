//! Compiler-owned assembly of graph-node inventory submissions.

use std::collections::{HashMap, HashSet};

use logic_analyzer_graph_api::node::{GraphNodeRegistration, RuntimeBuilder};
use node_graph::NodeTypeRegistry;

pub(crate) fn graph_node_registrations() -> Vec<&'static GraphNodeRegistration> {
    let mut registrations = inventory::iter::<GraphNodeRegistration>
        .into_iter()
        .collect::<Vec<_>>();
    validate_graph_node_registrations(&mut registrations);
    registrations
}

pub fn build_node_registry() -> NodeTypeRegistry {
    let mut registry = NodeTypeRegistry::new();
    for registration in graph_node_registrations() {
        assert!(
            registry.category_of(registration.name()).is_none(),
            "graph-node inventory definition '{}' conflicts with an explicit catalog entry",
            registration.name()
        );
        registration.apply_node(&mut registry);
    }
    registry
}

pub(crate) fn standard_graph_node_builders() -> HashMap<String, Box<dyn RuntimeBuilder>> {
    let mut builders: HashMap<String, Box<dyn RuntimeBuilder>> = HashMap::new();
    builders.insert(
        super::DATA_COLLECTOR_BUILDER.into(),
        Box::new(super::DataCollectorBuilder),
    );
    for registration in graph_node_registrations() {
        registration.apply_runtime_setup();
        let Some(builder) = registration.builder() else {
            continue;
        };
        assert!(
            builders
                .insert(registration.name().to_owned(), builder)
                .is_none(),
            "graph-node inventory builder '{}' conflicts with an explicit catalog entry",
            registration.name()
        );
    }
    builders
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
mod graph_node_registration_tests {
    use super::*;

    #[test]
    fn registrations_are_stably_ordered_and_unique() {
        let registrations = graph_node_registrations();
        assert!(registrations.windows(2).all(|pair| {
            pair[0].stable_id() < pair[1].stable_id() && pair[0].name() != pair[1].name()
        }));
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let registration = graph_node_registrations()[0];
        let mut registrations = vec![registration, registration];
        assert!(
            std::panic::catch_unwind(move || {
                validate_graph_node_registrations(&mut registrations)
            })
            .is_err()
        );
    }
}
