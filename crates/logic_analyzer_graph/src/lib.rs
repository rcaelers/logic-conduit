//! Logic-analyzer node catalog and graph-to-runtime compiler.
//!
//! This crate is the product-specific bridge between a generic [`node_graph`]
//! document and the UI-independent [`signal_processing`] runtime. Concrete
//! node viewer-lane adapters also live here; application composition and
//! window integration belong in `logic-analyzer-ui`.

mod collected_payloads;
mod compiler;
mod decoder_table;
pub mod host;
pub mod node;
pub mod node_support;
pub mod nodes;

#[cfg(not(target_arch = "wasm32"))]
mod capture_export;
#[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
pub mod test_support;

#[cfg(not(target_arch = "wasm32"))]
pub use capture_export::{
    CaptureExportDescriptor, CaptureExportFormat, CaptureExportObserver, CaptureExportProgress,
    CaptureExportReport, export_finalized_capture,
};
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
