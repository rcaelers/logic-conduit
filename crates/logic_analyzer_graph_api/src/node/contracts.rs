use std::sync::Arc;

use serde_json::Value;

use logic_analyzer_viewer::ViewerOutputPresentation;
use node_graph::Socket;
use signal_processing::{
    AcquisitionContext, AcquisitionError, AcquisitionResult, CaptureChannelId,
    CaptureProviderCapabilities, CaptureSessionPlan, CaptureStartMode, CaptureStoreCursor,
    DerivedDataRetention, NodeConfig, PreparedAcquisition, ProcessNode, TriggerProgram,
};

use crate::node_support::{
    CaptureCacheIdentity, CapturePresentation, DecoderTableColumnPresentation, LiveCaptureEdit,
    NodeBuildContext, PortKind, ResolvedInputs, SamplingOverlayDescriptor, SimpleTriggerChannel,
    TriggerConfigurationFeature,
};

pub trait CaptureGraphSourceFactory: Send + Sync {
    fn create(&self, cursor: Box<dyn CaptureStoreCursor>) -> Result<Box<dyn ProcessNode>, String>;
}

pub trait LiveCaptureFeature: Send {
    fn channels(&self) -> &[CaptureChannelId];
    fn channel_names(&self) -> &[String];
    fn sample_rate_hz(&self) -> f64;
    fn capabilities(&self) -> &CaptureProviderCapabilities;
    fn simple_trigger_channels(&self) -> &[SimpleTriggerChannel] {
        &[]
    }
    fn trigger_program(&self) -> Option<&TriggerProgram> {
        None
    }
    fn session_plan(&self) -> Option<&CaptureSessionPlan> {
        None
    }
    fn graph_source_factory(&self) -> Arc<dyn CaptureGraphSourceFactory>;
    fn prepare(
        self: Box<Self>,
        context: AcquisitionContext,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>>;
    fn prepare_with_mode(
        self: Box<Self>,
        context: AcquisitionContext,
        mode: CaptureStartMode,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        if mode == CaptureStartMode::CaptureNow {
            return Err(AcquisitionError::UnsupportedOperation("capture now".into()));
        }
        self.prepare(context)
    }
}

pub trait RuntimeBuilder {
    fn is_source(&self) -> bool {
        false
    }
    fn derived_data_retention(&self, _state: &Value) -> DerivedDataRetention {
        DerivedDataRetention::Unlimited
    }
    fn is_sink(&self) -> bool {
        false
    }
    fn is_data_subscription(&self) -> bool {
        false
    }
    fn is_data_collector(&self) -> bool {
        false
    }
    fn collected_lane_names(
        &self,
        _state: &Value,
        _resolved: &ResolvedInputs,
    ) -> Vec<(usize, String)> {
        Vec::new()
    }
    fn register_presentations(
        &self,
        _name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _lane_names: &[(usize, String)],
        _ctx: &dyn NodeBuildContext,
    ) -> Result<(), String> {
        Ok(())
    }
    fn accepted_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind>;
    fn offered_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind>;
    /// Optional owner-defined semantic contracts carried by an output.
    /// Empty means the payload type alone defines compatibility.
    fn offered_connection_contracts(&self, _socket: &Socket, _state: &Value) -> Vec<String> {
        Vec::new()
    }
    /// Optional owner-defined semantic contracts accepted by an input.
    /// When both ends declare contracts, at least one identity must match.
    fn accepted_connection_contracts(&self, _socket: &Socket, _state: &Value) -> Vec<String> {
        Vec::new()
    }
    fn input_port(
        &self,
        socket: &Socket,
        member_index: usize,
        state: &Value,
        kind: PortKind,
    ) -> Option<String>;
    fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String>;
    fn word_display_format(&self, _socket: &Socket, _state: &Value) -> Option<String> {
        None
    }
    fn viewer_output_presentation(
        &self,
        _socket: &Socket,
        _state: &Value,
    ) -> Option<ViewerOutputPresentation> {
        None
    }
    fn decoder_table_column(
        &self,
        _socket: &Socket,
        _state: &Value,
    ) -> Option<DecoderTableColumnPresentation> {
        None
    }
    fn viewer_channel_origin(&self, _socket: &Socket, _state: &Value) -> Option<usize> {
        None
    }
    fn capture_presentation(&self, _state: &Value) -> Result<Option<CapturePresentation>, String> {
        Ok(None)
    }
    fn capture_cache_identity(
        &self,
        _state: &Value,
        _resolved: &ResolvedInputs,
    ) -> CaptureCacheIdentity {
        CaptureCacheIdentity::NotCapture
    }
    fn sampling_overlay(&self, _state: &Value) -> Option<SamplingOverlayDescriptor> {
        None
    }
    fn live_capture_feature(
        &self,
        _state: &Value,
    ) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
        Ok(None)
    }
    fn trigger_configuration(
        &self,
        _state: &Value,
    ) -> Result<Option<TriggerConfigurationFeature>, String> {
        Ok(None)
    }
    fn apply_live_capture_edit(
        &self,
        _state: &Value,
        _edit: &LiveCaptureEdit,
    ) -> Result<Option<Value>, String> {
        Ok(None)
    }
    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        true
    }
    fn input_buffer_override(&self, _socket: &Socket, _state: &Value) -> Option<usize> {
        None
    }
    fn build(
        &self,
        _name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut dyn NodeBuildContext,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Err("graph-only builder has no runtime node".to_owned())
    }
    fn hot_config(&self, _state: &Value) -> Option<NodeConfig> {
        None
    }
}
