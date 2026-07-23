use std::path::PathBuf;

use logic_analyzer_graph::{BuilderRegistry, discover_live_capture_feature};
use node_graph::{GraphState, NodeId};
use signal_processing::{
    CaptureAcquisitionPhase, CaptureCommandCapabilities, CaptureCompletion, CaptureHealth,
    CaptureIndex, CaptureProgress, CaptureProviderCapabilities, CaptureSessionId,
    CaptureSessionOutcome, CaptureSessionPlan, CaptureSessionState, ProcessNode,
};

use super::platform::CaptureCoordinator;

/// Outer `Option` on the coordinator method means "no update"; this inner
/// option carries either a new growing index or an explicit detach.
pub(crate) type CaptureWaveformUpdate = Option<Box<dyn CaptureIndex>>;

pub(crate) struct CaptureAnalysisAttachment {
    pub(crate) source_node: NodeId,
    pub(crate) process: Box<dyn ProcessNode>,
}

/// Fresh source process for re-analyzing one immutable finalized session.
pub(crate) struct CaptureReplayAttachment {
    pub(crate) source_node: NodeId,
    pub(crate) process: Box<dyn ProcessNode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CaptureAvailability {
    Available {
        source_node: NodeId,
        source_title: String,
        has_trigger_program: bool,
        capabilities: CaptureProviderCapabilities,
        session_plan: Option<Box<CaptureSessionPlan>>,
    },
    Unavailable {
        reason: String,
    },
}

impl CaptureAvailability {
    pub(crate) fn reason(&self) -> Option<&str> {
        match self {
            Self::Available { .. } => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }
}

pub(crate) fn capture_availability(
    graph: &GraphState,
    builders: &BuilderRegistry,
) -> CaptureAvailability {
    if !CaptureCoordinator::backend_available() {
        return CaptureAvailability::Unavailable {
            reason: CaptureCoordinator::backend_unavailable_reason().into(),
        };
    }
    match discover_live_capture_feature(graph, builders) {
        Ok(Some(feature)) => CaptureAvailability::Available {
            source_node: feature.source_node(),
            source_title: feature.source_title().to_owned(),
            has_trigger_program: feature.has_trigger_program(),
            capabilities: feature.capabilities().clone(),
            session_plan: feature.session_plan().cloned().map(Box::new),
        },
        Ok(None) => CaptureAvailability::Unavailable {
            reason: "The graph has no live capture source".into(),
        },
        Err(error) => CaptureAvailability::Unavailable {
            reason: error.message,
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureSessionStatus {
    pub(crate) session_id: CaptureSessionId,
    pub(crate) source_node: NodeId,
    pub(crate) source_title: String,
    pub(crate) state: CaptureSessionState,
    pub(crate) phase: CaptureAcquisitionPhase,
    pub(crate) progress: CaptureProgress,
    pub(crate) health: CaptureHealth,
    pub(crate) commands: CaptureCommandCapabilities,
    pub(crate) session_plan: Option<CaptureSessionPlan>,
    pub(crate) trigger_sample: Option<u64>,
    pub(crate) recording_origin: Option<u64>,
    pub(crate) outcome: CaptureSessionOutcome,
    pub(crate) completion: Option<CaptureCompletion>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureExportStatus {
    pub(crate) format_label: String,
    pub(crate) destination: PathBuf,
    pub(crate) samples_written: u64,
    pub(crate) total_samples: u64,
    pub(crate) cancelling: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureExportCompletion {
    pub(crate) destination: PathBuf,
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct PreparedConfigurationEpoch {
    pub(crate) epoch_id: u64,
    pub(crate) source_sample: u64,
    pub(crate) boundary: signal_processing::ConfigurationBoundary,
    pub(crate) graph: node_graph::GraphState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigurationEpochResolution {
    Applied,
    Deferred(String),
    Failed(String),
}

pub(crate) trait CaptureCoordinatorContract {
    fn backend_available() -> bool;
    fn backend_unavailable_reason() -> &'static str;
    fn request_stop(&mut self);
    fn request_abort(&mut self) -> Result<(), String>;
    fn request_force_trigger(&mut self) -> Result<(), String>;
    fn set_graph_processed_samples(&mut self, processed_samples: Option<u64>);
    fn poll(&mut self);
    fn status(&self) -> Option<&CaptureSessionStatus>;
    fn take_waveform_update(&mut self) -> Option<CaptureWaveformUpdate>;
    fn take_analysis_attachment(&mut self) -> Option<CaptureAnalysisAttachment>;
    fn request_configuration_epoch(
        &mut self,
        _graph: node_graph::GraphState,
    ) -> Result<(), String> {
        Err("live configuration epochs are unavailable on this platform".into())
    }
    fn take_configuration_epoch_preparation(
        &mut self,
    ) -> Option<Result<PreparedConfigurationEpoch, String>> {
        None
    }
    fn resolve_configuration_epoch(
        &mut self,
        _epoch_id: u64,
        _resolution: ConfigurationEpochResolution,
    ) -> Result<(), String> {
        Err("live configuration epochs are unavailable on this platform".into())
    }
    fn take_configuration_epoch_notice(&mut self) -> Option<Result<(), String>> {
        None
    }
    fn replay_source_node(&self) -> Option<NodeId>;
    fn create_replay_attachment(&self) -> Result<Option<CaptureReplayAttachment>, String>;
    /// Remains true through Error cleanup until the supervisor has returned.
    fn is_active(&self) -> bool;

    fn graph_editing_enabled(&self) -> bool {
        !self.is_active()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use logic_analyzer_graph::nodes;
    use node_graph::NodeGraphWidget;

    use super::*;

    #[test]
    fn source_only_graph_is_available_for_raw_capture() {
        let mut graph = NodeGraphWidget::new(nodes::build_registry());
        graph
            .add_node_at(nodes::test_live_capture_source_name(), egui::Pos2::ZERO)
            .expect("test capture source is registered");

        assert!(matches!(
            capture_availability(graph.graph(), &BuilderRegistry::standard()),
            CaptureAvailability::Available { .. }
        ));
    }

    #[test]
    fn preloaded_demo_capture_is_not_a_live_capture_source() {
        let mut graph = NodeGraphWidget::new(nodes::build_registry());
        graph
            .add_node_at(nodes::test_capture_source_name(), egui::Pos2::ZERO)
            .expect("test capture source is registered");

        assert!(matches!(
            capture_availability(graph.graph(), &BuilderRegistry::standard()),
            CaptureAvailability::Unavailable { .. }
        ));
    }
}
