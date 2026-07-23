use std::collections::HashMap;
use std::path::Path;

use node_graph::{GraphState, NodeId, NodeTypeRegistry};
use signal_processing::{CollectedPayloadRegistry, ConfigurationBoundary, PersistentStoreConfig};

use super::errors::{ApplyError, CompileError};
use super::graph::{
    ApplySummary, BuilderRegistry, CompileCtx, CompiledGraph, DiscoveredCapturePresentation,
    DiscoveredLiveCaptureFeature, DiscoveredTriggerConfiguration, LiveAnalysisSource,
    LiveCaptureDiscoveryError, LiveRun, SamplingOverlayCandidate, SourceProcessOverrides,
};
use super::saved_graph::GraphCompatibilityWarning;
use super::{graph, graph_node_registration, saved_graph};
use crate::node_support::LiveCaptureEdit;

/// Stateful application-facing facade for graph discovery, compilation, and execution.
///
/// The compiler owns its inventory-derived runtime registry. Hosts supply graph documents and
/// consume resolved results without coordinating individual compiler functions or handling node
/// builders directly.
pub struct GraphCompiler {
    builders: BuilderRegistry,
}

impl GraphCompiler {
    pub fn new() -> Self {
        Self {
            builders: BuilderRegistry::standard(),
        }
    }

    pub fn build_node_registry(&self) -> NodeTypeRegistry {
        graph_node_registration::build_node_registry()
    }

    pub fn collected_payloads(&self) -> &CollectedPayloadRegistry {
        self.builders.collected_payloads()
    }

    pub fn discover_capture_presentation(
        &self,
        graph: &GraphState,
    ) -> Result<Option<DiscoveredCapturePresentation>, String> {
        graph::discover_capture_presentation(graph, &self.builders)
    }

    pub fn discover_live_capture_feature(
        &self,
        graph: &GraphState,
    ) -> Result<Option<DiscoveredLiveCaptureFeature>, LiveCaptureDiscoveryError> {
        graph::discover_live_capture_feature(graph, &self.builders)
    }

    pub fn discover_trigger_configuration(
        &self,
        graph: &GraphState,
    ) -> Result<Option<DiscoveredTriggerConfiguration>, LiveCaptureDiscoveryError> {
        graph::discover_trigger_configuration(graph, &self.builders)
    }

    pub fn apply_live_capture_edit(
        &self,
        graph: &GraphState,
        source_node: NodeId,
        edit: &LiveCaptureEdit,
    ) -> Result<serde_json::Value, String> {
        graph::apply_live_capture_edit(graph, &self.builders, source_node, edit)
    }

    pub fn synchronize_payload_subscriptions(
        &self,
        graph: &mut GraphState,
    ) -> Result<Vec<GraphCompatibilityWarning>, serde_json::Error> {
        saved_graph::synchronize_payload_subscriptions(graph, &self.builders)
    }

    pub fn lower(&self, graph: &GraphState) -> Result<CompiledGraph, Vec<CompileError>> {
        graph::lower(graph, &self.builders)
    }

    pub fn sampling_overlay_candidates(
        &self,
        graph: &GraphState,
    ) -> Result<Vec<SamplingOverlayCandidate>, Vec<CompileError>> {
        graph::sampling_overlay_candidates(graph, &self.builders)
    }

    pub fn derived_cache_configs_by_node(
        &self,
        graph: &GraphState,
        directory: &Path,
    ) -> Result<HashMap<NodeId, Vec<PersistentStoreConfig>>, Vec<CompileError>> {
        graph::derived_cache_configs_by_node(graph, &self.builders, directory)
    }

    pub fn start_app_run(
        &self,
        graph: &GraphState,
        ctx: &mut CompileCtx,
    ) -> Result<LiveRun, Vec<CompileError>> {
        graph::start_app_run(graph, &self.builders, ctx)
    }

    pub fn start_app_run_with_source_overrides(
        &self,
        graph: &GraphState,
        ctx: &mut CompileCtx,
        overrides: SourceProcessOverrides,
    ) -> Result<LiveRun, Vec<CompileError>> {
        graph::start_app_run_with_source_overrides(graph, &self.builders, ctx, overrides)
    }

    pub fn start_live_analysis(
        &self,
        graph: &GraphState,
        ctx: &mut CompileCtx,
        source: LiveAnalysisSource,
    ) -> Result<LiveRun, Vec<CompileError>> {
        graph::start_live_analysis(graph, &self.builders, ctx, source)
    }

    pub fn apply_run(
        &self,
        run: &mut LiveRun,
        graph: &GraphState,
    ) -> Result<ApplySummary, ApplyError> {
        run.apply(graph, &self.builders)
    }

    pub fn apply_configuration_epoch(
        &self,
        run: &mut LiveRun,
        graph: &GraphState,
        boundary: ConfigurationBoundary,
    ) -> Result<ApplySummary, ApplyError> {
        run.apply_configuration_epoch(graph, &self.builders, boundary)
    }
}

impl Default for GraphCompiler {
    fn default() -> Self {
        Self::new()
    }
}
