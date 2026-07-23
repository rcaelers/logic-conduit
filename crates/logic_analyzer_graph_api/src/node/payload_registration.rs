use std::sync::Arc;

use signal_processing::{CollectedLaneRequest, CollectedPayloadAdapter};

use crate::node_support::{
    DefaultViewerPayloadPresentation, NodeBuildContext, PortKind, PortValue, ResolvedInput,
};

pub type CollectedPayloadRequestConfigurator =
    fn(CollectedLaneRequest, usize, &ResolvedInput, &dyn NodeBuildContext) -> CollectedLaneRequest;

pub struct CollectedPayloadRegistration {
    stable_id: &'static str,
    kind: fn() -> PortKind,
    adapter: fn() -> Arc<dyn CollectedPayloadAdapter>,
    presentation: fn() -> DefaultViewerPayloadPresentation,
    configure_request: CollectedPayloadRequestConfigurator,
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
        configure_request: CollectedPayloadRequestConfigurator,
        persistent_cache: bool,
    ) -> Self {
        Self {
            stable_id,
            kind: PortKind::of::<T>,
            adapter,
            presentation,
            configure_request,
            persistent_cache,
        }
    }

    pub const fn stable_id(&self) -> &'static str {
        self.stable_id
    }

    pub fn kind(&self) -> PortKind {
        (self.kind)()
    }

    #[doc(hidden)]
    pub fn adapter(&self) -> Arc<dyn CollectedPayloadAdapter> {
        (self.adapter)()
    }

    #[doc(hidden)]
    pub fn presentation(&self) -> DefaultViewerPayloadPresentation {
        (self.presentation)()
    }

    #[doc(hidden)]
    pub const fn configure_request(&self) -> CollectedPayloadRequestConfigurator {
        self.configure_request
    }

    #[doc(hidden)]
    pub const fn persistent_cache(&self) -> bool {
        self.persistent_cache
    }
}

fn identity_request(
    request: CollectedLaneRequest,
    _member: usize,
    _input: &ResolvedInput,
    _ctx: &dyn NodeBuildContext,
) -> CollectedLaneRequest {
    request
}

inventory::collect!(CollectedPayloadRegistration);
