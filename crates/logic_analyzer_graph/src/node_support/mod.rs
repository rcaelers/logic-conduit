//! Values and build-time services supplied to graph-node implementations.

pub use logic_analyzer_graph_api::node_support::{
    CaptureCacheIdentity, CapturePresentation, CapturePresentationSignal, DecoderTableCellMode,
    DecoderTableColumnPresentation, DefaultViewerPayloadPresentation, LiveCaptureEdit,
    NodeBuildContext, PortKind, PortValue, ResolvedInput, ResolvedInputs,
    SamplingOverlayDescriptor, SamplingQualifierDescriptor, SimpleTriggerChannel,
    TriggerConfigurationFeature,
};
