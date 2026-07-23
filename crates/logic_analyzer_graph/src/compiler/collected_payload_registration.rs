//! Compiler-owned assembly of collected-payload inventory submissions.

use std::collections::HashSet;
use std::sync::Arc;

use logic_analyzer_graph_api::node::CollectedPayloadRegistration;
use signal_processing::CollectedPayloadRegistrationError;

use super::graph::{BuilderRegistry, CollectedPayloadSubscription};

pub(crate) fn collected_payload_registrations() -> Vec<&'static CollectedPayloadRegistration> {
    let mut registrations = inventory::iter::<CollectedPayloadRegistration>
        .into_iter()
        .collect::<Vec<_>>();
    validate_collected_payload_registrations(&mut registrations);
    registrations
}

pub(crate) fn apply_collected_payload_registration(
    registration: &CollectedPayloadRegistration,
    registry: &mut BuilderRegistry,
) -> Result<(), CollectedPayloadRegistrationError> {
    let kind = registration.kind();
    kind.register_runtime_type();
    registry
        .collected_payloads
        .register_erased(kind.type_id(), registration.stable_id())?;
    registry.collected_payloads.register_adapter_erased(
        kind.type_id(),
        kind.name(),
        registration.adapter(),
    )?;
    registry
        .payload_subscriptions
        .push(CollectedPayloadSubscription {
            kind,
            diagnostic_name: kind.name().to_owned(),
            presentation: registration.presentation(),
            persistent_cache: registration.persistent_cache(),
            configure_request: Arc::new(registration.configure_request()),
        });
    Ok(())
}

fn validate_collected_payload_registrations(
    registrations: &mut Vec<&CollectedPayloadRegistration>,
) {
    registrations.sort_by_key(|registration| registration.stable_id());
    let mut stable_ids = HashSet::new();
    let mut type_ids = HashSet::new();
    for registration in registrations {
        assert!(
            !registration.stable_id().trim().is_empty(),
            "collected-payload inventory contains an empty stable ID"
        );
        assert!(
            stable_ids.insert(registration.stable_id()),
            "duplicate collected-payload inventory stable ID '{}'",
            registration.stable_id()
        );
        assert!(
            type_ids.insert(registration.kind().type_id()),
            "duplicate collected-payload inventory type '{}'",
            registration.kind().name()
        );
    }
}

#[cfg(test)]
mod collected_payload_registration_tests {
    use super::*;

    #[test]
    fn registrations_are_stably_ordered_and_unique() {
        let registrations = collected_payload_registrations();
        assert!(
            registrations
                .windows(2)
                .all(|pair| pair[0].stable_id() < pair[1].stable_id())
        );
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let registration = collected_payload_registrations()[0];
        let mut registrations = vec![registration, registration];
        assert!(
            std::panic::catch_unwind(move || {
                validate_collected_payload_registrations(&mut registrations)
            })
            .is_err()
        );
    }
}
