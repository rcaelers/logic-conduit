//! Logic-analyzer node catalog and graph-to-runtime compiler.
//!
//! This crate is the product-specific bridge between a generic [`node_graph`]
//! document and the UI-independent [`signal_processing`] runtime. Concrete
//! node viewer-lane adapters also live here; application composition and
//! window integration belong in `logic-analyzer-ui`.

mod compiler;
pub mod nodes;

pub(crate) use compiler::parse_state;
pub use compiler::{
    AppRun, ApplyError, ApplySummary, BuilderRegistry, CaptureGraphSourceFactory, CompileCtx,
    CompileError, CompiledEdge, CompiledGraph, CompiledNode, DiscoveredLiveCaptureFeature,
    DiscoveredTriggerConfiguration, LiveAnalysisSource, LiveCaptureDiscoveryError, LiveCaptureEdit,
    LiveCaptureFeature, LiveRun, PluginContext, PortKind, PortValue, ResolvedInput, ResolvedInputs,
    RuntimeBuilder, SamplingOverlayCandidate, SamplingOverlayDescriptor,
    SamplingQualifierDescriptor, SimpleTriggerChannel, SourceProcessOverrides,
    TriggerConfigurationFeature, apply_live_capture_edit, derived_cache_configs_by_node,
    discover_compiled_live_capture_feature, discover_live_capture_feature,
    discover_trigger_configuration, lower, sampling_overlay_candidates, start_app_run,
    start_app_run_with_source_overrides, start_live, start_live_analysis,
};
