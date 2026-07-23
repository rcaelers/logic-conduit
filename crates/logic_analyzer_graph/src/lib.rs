//! Logic-analyzer graph-to-runtime compiler and application-host services.
//!
//! This crate lowers a generic [`node_graph`] document through inventory-submitted node contracts
//! into the UI-independent [`signal_processing`] runtime. Concrete graph nodes and their
//! presentations live in `logic-analyzer-graph-nodes`; application composition and window
//! integration belong in `logic-analyzer-ui`.

mod compiler;
mod decoder_table;
pub mod host;
pub mod node;
pub mod node_support;
#[cfg(test)]
mod nodes;

pub use compiler::{
    ApplyError, ApplySummary, BuilderRegistry, CaptureCacheIdentity, CaptureGraphSourceFactory,
    CapturePresentation, CapturePresentationSignal, CollectedPayloadRegistration, CompileCtx,
    CompileError, CompiledEdge, CompiledGraph, CompiledNode, DefaultViewerPayloadPresentation,
    DiscoveredCapturePresentation, DiscoveredLiveCaptureFeature, DiscoveredTriggerConfiguration,
    GraphCompatibilityWarning, GraphNodeRegistration, LiveAnalysisSource,
    LiveCaptureDiscoveryError, LiveCaptureEdit, LiveCaptureFeature, LiveRun, NodeBuildContext,
    PortKind, PortValue, ResolvedInput, ResolvedInputs, RuntimeBuilder, SamplingOverlayCandidate,
    SamplingOverlayDescriptor, SamplingQualifierDescriptor, SimpleTriggerChannel,
    SourceProcessOverrides, TriggerConfigurationFeature, apply_live_capture_edit,
    build_node_registry, derived_cache_configs_by_node, discover_capture_presentation,
    discover_live_capture_feature, discover_trigger_configuration, lower,
    sampling_overlay_candidates, start_app_run, start_app_run_with_source_overrides,
    start_live_analysis, synchronize_payload_subscriptions,
};
pub use decoder_table::{
    DecoderTableCellMode, DecoderTableColumn, DecoderTableColumnPresentation, DecoderTableRegistry,
    DecoderTableSource,
};
