mod collected_payload_registration;
mod data_collector;
mod errors;
mod graph;
mod graph_compiler;
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
    ApplySummary, CompileCtx, CompiledEdge, CompiledGraph, CompiledNode,
    DiscoveredCapturePresentation, DiscoveredLiveCaptureFeature, DiscoveredTriggerConfiguration,
    LiveAnalysisSource, LiveCaptureDiscoveryError, LiveRun, SamplingOverlayCandidate,
    SourceProcessOverrides,
};
pub use graph_compiler::GraphCompiler;
pub(crate) use graph_node_registration::{
    standard_graph_node_builders, validate_graph_node_payload_requirements,
};
pub use saved_graph::GraphCompatibilityWarning;
