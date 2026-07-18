use logic_analyzer_graph::compiler::DiscoveredLiveCaptureFeature;

use super::{
    CaptureAnalysisAttachment, CaptureCoordinatorContract, CaptureReplayAttachment,
    CaptureSessionStatus, CaptureWaveformUpdate,
};

pub(crate) struct CaptureCoordinator;

impl CaptureCoordinator {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl CaptureCoordinatorContract for CaptureCoordinator {
    fn backend_available() -> bool {
        false
    }

    fn backend_unavailable_reason() -> &'static str {
        "Live capture is not available in this web build"
    }

    fn start(&mut self, _feature: DiscoveredLiveCaptureFeature) -> Result<(), String> {
        Err(Self::backend_unavailable_reason().into())
    }

    fn request_stop(&mut self) {}

    fn poll(&mut self) {}

    fn status(&self) -> Option<&CaptureSessionStatus> {
        None
    }

    fn take_waveform_update(&mut self) -> Option<CaptureWaveformUpdate> {
        None
    }

    fn take_analysis_attachment(&mut self) -> Option<CaptureAnalysisAttachment> {
        None
    }

    fn replay_source_node(&self) -> Option<node_graph::NodeId> {
        None
    }

    fn create_replay_attachment(&self) -> Result<Option<CaptureReplayAttachment>, String> {
        Ok(None)
    }

    fn is_active(&self) -> bool {
        false
    }
}
