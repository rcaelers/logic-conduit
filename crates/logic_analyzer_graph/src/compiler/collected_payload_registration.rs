//! Inventory contract for one complete collected-payload capability.

use std::any::TypeId;
use std::collections::HashSet;
use std::sync::Arc;

use signal_processing::{
    CollectedLaneRequest, CollectedPayloadAdapter, CollectedPayloadRegistrationError,
};

use super::graph::{BuilderRegistry, CompileCtx, DefaultViewerPayloadPresentation, ResolvedInput};
use super::port_kind::PortValue;

type RequestConfigurator =
    fn(CollectedLaneRequest, usize, &ResolvedInput, &CompileCtx) -> CollectedLaneRequest;

/// One independently discoverable retained payload, including ingestion,
/// query/storage, and its default waveform presentation.
pub struct CollectedPayloadRegistration {
    stable_id: &'static str,
    payload_type_id: fn() -> TypeId,
    payload_name: fn() -> &'static str,
    apply: fn(
        &CollectedPayloadRegistration,
        &mut BuilderRegistry,
    ) -> Result<(), CollectedPayloadRegistrationError>,
    adapter: fn() -> Arc<dyn CollectedPayloadAdapter>,
    presentation: fn() -> DefaultViewerPayloadPresentation,
    configure_request: RequestConfigurator,
    persistent_cache: bool,
}

impl CollectedPayloadRegistration {
    pub const fn subscribable<T: PortValue>(
        stable_id: &'static str,
        adapter: fn() -> Arc<dyn CollectedPayloadAdapter>,
        presentation: fn() -> DefaultViewerPayloadPresentation,
    ) -> Self {
        Self::subscribable_with_request_configurator::<T>(
            stable_id,
            adapter,
            presentation,
            identity_request,
            false,
        )
    }

    pub const fn subscribable_with_request_configurator<T: PortValue>(
        stable_id: &'static str,
        adapter: fn() -> Arc<dyn CollectedPayloadAdapter>,
        presentation: fn() -> DefaultViewerPayloadPresentation,
        configure_request: RequestConfigurator,
        persistent_cache: bool,
    ) -> Self {
        Self {
            stable_id,
            payload_type_id: payload_type_id::<T>,
            payload_name: T::kind_name,
            apply: apply_registration::<T>,
            adapter,
            presentation,
            configure_request,
            persistent_cache,
        }
    }

    pub const fn stable_id(&self) -> &'static str {
        self.stable_id
    }

    pub fn payload_type_id(&self) -> TypeId {
        (self.payload_type_id)()
    }

    pub fn payload_name(&self) -> &'static str {
        (self.payload_name)()
    }

    pub(crate) fn apply_to(
        &self,
        registry: &mut BuilderRegistry,
    ) -> Result<(), CollectedPayloadRegistrationError> {
        (self.apply)(self, registry)
    }
}

fn payload_type_id<T: PortValue>() -> TypeId {
    TypeId::of::<T>()
}

fn identity_request(
    request: CollectedLaneRequest,
    _member: usize,
    _input: &ResolvedInput,
    _ctx: &CompileCtx,
) -> CollectedLaneRequest {
    request
}

fn apply_registration<T: PortValue>(
    registration: &CollectedPayloadRegistration,
    registry: &mut BuilderRegistry,
) -> Result<(), CollectedPayloadRegistrationError> {
    registry.register_collected_payload_adapter::<T>(
        registration.stable_id,
        (registration.adapter)(),
    )?;
    registry.register_collected_payload_subscription_with_request_configurator::<T>(
        (registration.presentation)(),
        Arc::new(registration.configure_request),
        registration.persistent_cache,
    )?;
    Ok(())
}

inventory::collect!(CollectedPayloadRegistration);

pub(crate) fn collected_payload_registrations() -> Vec<&'static CollectedPayloadRegistration> {
    let mut registrations = inventory::iter::<CollectedPayloadRegistration>
        .into_iter()
        .collect::<Vec<_>>();
    registrations.sort_by_key(|registration| registration.stable_id());

    let mut stable_ids = HashSet::new();
    let mut type_ids = HashSet::new();
    for registration in &registrations {
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
            type_ids.insert(registration.payload_type_id()),
            "duplicate collected-payload inventory type '{}'",
            registration.payload_name()
        );
    }
    registrations
}

#[cfg(test)]
mod collected_payload_registration_tests {
    use super::*;

    #[test]
    fn built_in_payload_capabilities_are_complete_and_stably_ordered() {
        let registrations = collected_payload_registrations();
        let stable_ids = registrations
            .iter()
            .map(|registration| registration.stable_id())
            .collect::<Vec<_>>();
        assert_eq!(
            stable_ids,
            [
                "org.logicconduit.digital-sample/v1",
                "org.logicconduit.number-sample/v1",
                "org.logicconduit.text-sample/v1",
                "org.logicconduit.trigger/v1",
                "org.logicconduit.word/v1",
            ]
        );

        let registry = BuilderRegistry::standard();
        for registration in registrations {
            let descriptor = registry
                .collected_payloads()
                .descriptor_by_stable_id(registration.stable_id())
                .expect("inventory payload must have a durable descriptor");
            assert!(
                registry
                    .collected_payloads()
                    .adapter_by_type_id(registration.payload_type_id())
                    .is_some(),
                "payload '{}' has no adapter",
                descriptor.stable_id()
            );
            assert!(registry.has_payload_subscription(registration.stable_id()));
        }
    }
}
