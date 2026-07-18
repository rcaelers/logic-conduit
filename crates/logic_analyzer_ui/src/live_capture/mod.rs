//! Application-level coordination for immediate live capture.

use logic_analyzer_graph::compiler::{
    BuilderRegistry, DiscoveredLiveCaptureFeature, discover_compiled_live_capture_feature, lower,
};
use node_graph::{GraphState, NodeId};
use signal_processing::{
    CaptureAcquisitionPhase, CaptureIndex, CaptureProgress, CaptureSessionId, CaptureSessionState,
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

/// Outer `Option` on the coordinator method means "no update"; this inner
/// option carries either a new growing index or an explicit detach.
pub(crate) type CaptureWaveformUpdate = Option<Box<dyn CaptureIndex>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CaptureAvailability {
    Available {
        source_node: NodeId,
        source_title: String,
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
            source_title: feature.source_title,
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
    pub error: Option<String>,
}

pub(crate) trait CaptureCoordinatorContract {
    fn backend_available() -> bool;
    fn backend_unavailable_reason() -> &'static str;
    fn start(&mut self, feature: DiscoveredLiveCaptureFeature) -> Result<(), String>;
    fn request_stop(&mut self);
    fn poll(&mut self);
    fn status(&self) -> Option<&CaptureSessionStatus>;
    fn take_waveform_update(&mut self) -> Option<CaptureWaveformUpdate>;
    /// Remains true through Error cleanup until the supervisor has returned.
    fn is_active(&self) -> bool;

    fn graph_editing_enabled(&self) -> bool {
        !self.is_active()
    }
}
