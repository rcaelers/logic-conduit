//! Application-level coordination for immediate live capture.

#[cfg(test)]
use logic_analyzer_graph::compiler::DiscoveredLiveCaptureFeature;
use logic_analyzer_graph::compiler::{
    BuilderRegistry, SimpleTriggerChannel, discover_compiled_live_capture_feature, lower,
};
use node_graph::{GraphState, NodeId};
#[cfg(test)]
use signal_processing::CaptureStartMode;
use signal_processing::{
    CaptureAcquisitionPhase, CaptureCommandCapabilities, CaptureCompletion, CaptureHealth,
    CaptureIndex, CaptureProgress, CaptureProviderCapabilities, CaptureSessionId,
    CaptureSessionOutcome, CaptureSessionPlan, CaptureSessionState, ProcessNode,
};

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "wasm.rs"]
        mod imp;
    }
    _ => {
        #[path = "native.rs"]
        mod imp;
    }
}

pub(crate) use imp::CaptureCoordinator;

#[cfg(test)]
mod architecture_tests;

/// Outer `Option` on the coordinator method means "no update"; this inner
/// option carries either a new growing index or an explicit detach.
pub(crate) type CaptureWaveformUpdate = Option<Box<dyn CaptureIndex>>;

pub(crate) struct CaptureAnalysisAttachment {
    pub source_node: NodeId,
    pub process: Box<dyn ProcessNode>,
}

/// Fresh source process for re-analyzing one immutable finalized session.
pub(crate) struct CaptureReplayAttachment {
    pub source_node: NodeId,
    pub process: Box<dyn ProcessNode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CaptureAvailability {
    Available {
        source_node: NodeId,
        source_title: String,
        simple_trigger_channels: Vec<SimpleTriggerChannel>,
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
    let compiled = match lower(graph, builders) {
        Ok(compiled) => compiled,
        Err(errors) => {
            let reason = errors
                .first()
                .map(|error| error.message.clone())
                .unwrap_or_else(|| "The graph is not valid for capture".into());
            return CaptureAvailability::Unavailable { reason };
        }
    };
    match discover_compiled_live_capture_feature(graph, &compiled, builders) {
        Ok(Some(feature)) => CaptureAvailability::Available {
            source_node: feature.source_node,
            source_title: feature.source_title.clone(),
            simple_trigger_channels: feature.simple_trigger_channels().to_vec(),
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
    pub session_id: CaptureSessionId,
    pub source_node: NodeId,
    pub source_title: String,
    pub state: CaptureSessionState,
    pub phase: CaptureAcquisitionPhase,
    pub progress: CaptureProgress,
    pub health: CaptureHealth,
    pub commands: CaptureCommandCapabilities,
    pub session_plan: Option<CaptureSessionPlan>,
    pub trigger_sample: Option<u64>,
    pub recording_origin: Option<u64>,
    pub outcome: CaptureSessionOutcome,
    pub completion: Option<CaptureCompletion>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecentCaptureSession {
    pub session_id: Option<CaptureSessionId>,
    pub outcome: CaptureSessionOutcome,
    pub committed_samples: u64,
    pub bytes: u64,
    pub kept: bool,
    pub recovered: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CaptureCleanupAdvisory {
    pub total_sessions: usize,
    pub total_bytes: u64,
    pub over_session_limit: usize,
    pub over_byte_limit: u64,
    pub discard_candidates: Vec<CaptureSessionId>,
}

pub(crate) trait CaptureCoordinatorContract {
    fn backend_available() -> bool;
    fn backend_unavailable_reason() -> &'static str;
    #[cfg(test)]
    fn start(
        &mut self,
        feature: DiscoveredLiveCaptureFeature,
        mode: CaptureStartMode,
    ) -> Result<(), String>;
    fn request_stop(&mut self);
    fn request_abort(&mut self) -> Result<(), String>;
    fn request_force_trigger(&mut self) -> Result<(), String>;
    fn set_graph_processed_samples(&mut self, processed_samples: Option<u64>);
    fn poll(&mut self);
    fn status(&self) -> Option<&CaptureSessionStatus>;
    fn take_waveform_update(&mut self) -> Option<CaptureWaveformUpdate>;
    fn take_analysis_attachment(&mut self) -> Option<CaptureAnalysisAttachment>;
    fn replay_source_node(&self) -> Option<NodeId>;
    fn create_replay_attachment(&self) -> Result<Option<CaptureReplayAttachment>, String>;
    /// Remains true through Error cleanup until the supervisor has returned.
    fn is_active(&self) -> bool;

    fn graph_editing_enabled(&self) -> bool {
        !self.is_active()
    }
}
