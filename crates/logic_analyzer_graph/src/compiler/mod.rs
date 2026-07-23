mod collected_payload_registration;
mod data_collector;
mod errors;
mod graph;
mod graph_node_registration;
mod saved_graph;

#[cfg(test)]
mod architecture_tests;

#[cfg(not(target_arch = "wasm32"))]
#[path = "cache_platform_native.rs"]
mod cache_platform;
#[cfg(target_arch = "wasm32")]
#[path = "cache_platform_wasm.rs"]
mod cache_platform;

pub(crate) use collected_payload_registration::collected_payload_registrations;
pub(crate) use data_collector::{BUILDER_NAME as DATA_COLLECTOR_BUILDER, DataCollectorBuilder};
pub use errors::{ApplyError, CompileError};
pub use graph::{
    ApplySummary, BuilderRegistry, CompileCtx, CompiledEdge, CompiledGraph, CompiledNode,
    DiscoveredCapturePresentation, DiscoveredLiveCaptureFeature, DiscoveredTriggerConfiguration,
    LiveAnalysisSource, LiveCaptureDiscoveryError, LiveRun, SamplingOverlayCandidate,
    SourceProcessOverrides, apply_live_capture_edit, derived_cache_configs_by_node,
    discover_capture_presentation, discover_live_capture_feature, discover_trigger_configuration,
    lower, sampling_overlay_candidates, start_app_run, start_app_run_with_source_overrides,
    start_live_analysis,
};
pub use graph_node_registration::build_node_registry;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use graph_node_registration::graph_node_registrations;
pub(crate) use graph_node_registration::{
    standard_graph_node_builders, validate_graph_node_payload_requirements,
};
pub use logic_analyzer_graph_api::node::{
    CaptureGraphSourceFactory, CollectedPayloadRegistration, GraphNodeRegistration,
    LiveCaptureFeature, RuntimeBuilder,
};
pub use logic_analyzer_graph_api::node_support::{
    CaptureCacheIdentity, CapturePresentation, CapturePresentationSignal,
    DefaultViewerPayloadPresentation, LiveCaptureEdit, NodeBuildContext, PortKind, PortValue,
    ResolvedInput, ResolvedInputs, SamplingOverlayDescriptor, SamplingQualifierDescriptor,
    SimpleTriggerChannel, TriggerConfigurationFeature,
};
pub use saved_graph::{GraphCompatibilityWarning, synchronize_payload_subscriptions};
