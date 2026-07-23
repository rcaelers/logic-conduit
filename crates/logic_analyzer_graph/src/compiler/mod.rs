mod collected_payload_registration;
mod data_collector;
mod errors;
mod graph;
mod port_kind;
mod saved_graph;

#[cfg(test)]
mod architecture_tests;

#[cfg(not(target_arch = "wasm32"))]
#[path = "cache_platform_native.rs"]
mod cache_platform;
#[cfg(target_arch = "wasm32")]
#[path = "cache_platform_wasm.rs"]
mod cache_platform;

pub use collected_payload_registration::CollectedPayloadRegistration;
pub(crate) use collected_payload_registration::collected_payload_registrations;
pub(crate) use data_collector::{BUILDER_NAME as DATA_COLLECTOR_BUILDER, DataCollectorBuilder};
pub use errors::{ApplyError, CompileError};
pub(crate) use graph::parse_state;
pub use graph::{
    ApplySummary, BuilderRegistry, CaptureCacheIdentity, CaptureGraphSourceFactory,
    CapturePresentation, CapturePresentationSignal, CompileCtx, CompiledEdge, CompiledGraph,
    CompiledNode, DefaultViewerPayloadPresentation, DiscoveredCapturePresentation,
    DiscoveredLiveCaptureFeature, DiscoveredTriggerConfiguration, LiveAnalysisSource,
    LiveCaptureDiscoveryError, LiveCaptureEdit, LiveCaptureFeature, LiveRun, NodeBuildContext,
    ResolvedInput, ResolvedInputs, RuntimeBuilder, SamplingOverlayCandidate,
    SamplingOverlayDescriptor, SamplingQualifierDescriptor, SimpleTriggerChannel,
    SourceProcessOverrides, TriggerConfigurationFeature, apply_live_capture_edit,
    derived_cache_configs_by_node, discover_capture_presentation, discover_live_capture_feature,
    discover_trigger_configuration, lower, sampling_overlay_candidates, start_app_run,
    start_app_run_with_source_overrides, start_live_analysis,
};
pub use port_kind::{PortKind, PortValue};
pub use saved_graph::{GraphCompatibilityWarning, synchronize_payload_subscriptions};
