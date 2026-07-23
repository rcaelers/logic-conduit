use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use logic_analyzer_viewer::{
    DefaultViewerLaneRenderer, SamplingEdge, ViewerLaneBadge, ViewerLaneGroup, ViewerLaneRenderer,
    ViewerOutputPresentation,
};
use node_graph::NodeId;
use signal_processing::{
    CaptureChannelId, CaptureIndexFactory, DerivedDataRetention, DerivedLanes,
    PersistentStoreConfig, SamplingActivity, SimpleTriggerCondition, TriggerEditorSchema,
    TriggerProgram,
};

use super::port::PortKind;

pub fn parse_state<T: serde::de::DeserializeOwned>(state: &serde_json::Value) -> Result<T, String> {
    serde_json::from_value(state.clone()).map_err(|error| format!("invalid node state: {error}"))
}

pub trait NodeBuildContext {
    fn derived_lanes(&self) -> &DerivedLanes;
    fn derived_data_retention(&self) -> DerivedDataRetention;
    fn derived_word_cache(&self, member: usize) -> Option<&PersistentStoreConfig>;
    fn register_waveform_presentation(&self, presentation: ViewerLaneGroup);
    fn sampling_activity(&self, runtime_name: &str, input: usize) -> Option<SamplingActivity>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamplingOverlayDescriptor {
    pub clock_input: usize,
    pub sampled_input_groups: Vec<usize>,
    pub edge: SamplingEdge,
    pub qualifiers: Vec<SamplingQualifierDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SamplingQualifierDescriptor {
    pub input: usize,
    pub active_level: bool,
    pub runtime_fallback: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedInput {
    pub kind: PortKind,
    pub source: String,
    pub source_node: NodeId,
    pub source_node_title: String,
    pub word_display_format: Option<String>,
    pub viewer_presentation: Option<ViewerOutputPresentation>,
    pub default_viewer_presentation: Option<DefaultViewerPayloadPresentation>,
    pub decoder_table_column: Option<DecoderTableColumnPresentation>,
    pub capture_channel: Option<usize>,
}

#[derive(Clone)]
pub struct DefaultViewerPayloadPresentation {
    badge: ViewerLaneBadge,
    renderer: Arc<dyn ViewerLaneRenderer>,
}

impl DefaultViewerPayloadPresentation {
    pub fn new(badge: ViewerLaneBadge) -> Self {
        Self::with_renderer(badge, Arc::new(DefaultViewerLaneRenderer))
    }

    pub fn with_renderer(badge: ViewerLaneBadge, renderer: Arc<dyn ViewerLaneRenderer>) -> Self {
        Self { badge, renderer }
    }

    pub fn badge(&self) -> &ViewerLaneBadge {
        &self.badge
    }

    pub fn renderer(&self) -> Arc<dyn ViewerLaneRenderer> {
        Arc::clone(&self.renderer)
    }
}

impl fmt::Debug for DefaultViewerPayloadPresentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DefaultViewerPayloadPresentation")
            .field("badge", &self.badge)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedInputs(HashMap<(usize, usize), ResolvedInput>);

impl ResolvedInputs {
    pub fn get(&self, def_index: usize, member_index: usize) -> Option<&ResolvedInput> {
        self.0.get(&(def_index, member_index))
    }

    pub fn kind(&self, def_index: usize) -> Option<PortKind> {
        self.0.get(&(def_index, 0)).map(|input| input.kind)
    }

    pub fn member_count(&self, def_index: usize) -> usize {
        self.0.keys().filter(|(def, _)| *def == def_index).count()
    }

    pub fn members(&self, def_index: usize) -> Vec<(usize, &ResolvedInput)> {
        let mut members = self
            .0
            .iter()
            .filter(|((def, _), _)| *def == def_index)
            .map(|((_, member), input)| (*member, input))
            .collect::<Vec<_>>();
        members.sort_by_key(|(member, _)| *member);
        members
    }

    #[doc(hidden)]
    pub fn insert(&mut self, def_index: usize, member_index: usize, input: ResolvedInput) {
        self.0.insert((def_index, member_index), input);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleTriggerChannel {
    pub channel_id: CaptureChannelId,
    pub viewer_channel: usize,
    pub name: String,
    pub enabled: bool,
    pub condition: SimpleTriggerCondition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerConfigurationFeature {
    schema: Arc<TriggerEditorSchema>,
    program: Option<TriggerProgram>,
    channels: Vec<SimpleTriggerChannel>,
}

impl TriggerConfigurationFeature {
    pub fn new(
        schema: TriggerEditorSchema,
        program: Option<TriggerProgram>,
        channels: Vec<SimpleTriggerChannel>,
    ) -> Result<Self, String> {
        let all_channel_ids = channels
            .iter()
            .map(|channel| channel.channel_id.clone())
            .collect::<Vec<_>>();
        let channel_ids = channels
            .iter()
            .filter(|channel| channel.enabled)
            .map(|channel| channel.channel_id.clone())
            .collect::<Vec<_>>();
        if all_channel_ids.iter().collect::<HashSet<_>>().len() != all_channel_ids.len() {
            return Err("trigger configuration channel identities must be unique".into());
        }
        if channels
            .iter()
            .map(|channel| channel.viewer_channel)
            .collect::<HashSet<_>>()
            .len()
            != channels.len()
        {
            return Err("trigger configuration viewer channels must be unique".into());
        }
        if let Some(program) = &program {
            schema
                .validate_program(program, &channel_ids)
                .map_err(|error| error.to_string())?;
        }
        Ok(Self {
            schema: Arc::new(schema),
            program,
            channels,
        })
    }

    pub fn schema(&self) -> &TriggerEditorSchema {
        &self.schema
    }

    pub fn program(&self) -> Option<&TriggerProgram> {
        self.program.as_ref()
    }

    pub fn channels(&self) -> &[SimpleTriggerChannel] {
        &self.channels
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveCaptureEdit {
    SetSimpleTrigger {
        channel_id: CaptureChannelId,
        condition: SimpleTriggerCondition,
    },
    SetTriggerProgram {
        program: Option<TriggerProgram>,
    },
}

pub struct CapturePresentationSignal {
    pub index: usize,
    pub name: String,
    pub initial: bool,
    pub transitions: Vec<(f64, bool)>,
}

pub enum CapturePresentation {
    Indexed {
        identity: PathBuf,
        factory: Box<dyn CaptureIndexFactory>,
    },
    InMemory {
        signals: Vec<CapturePresentationSignal>,
        duration_us: f64,
    },
    Channels(Vec<(usize, String)>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureCacheIdentity {
    #[default]
    NotCapture,
    Dynamic,
    Stable([u8; 32]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecoderTableCellMode {
    Single,
    Joined(String),
}

#[derive(Clone)]
pub struct DecoderTableColumnPresentation {
    pub source_key: String,
    pub column_key: String,
    pub label: String,
    pub order: usize,
    pub row_anchor: bool,
    pub cell_mode: DecoderTableCellMode,
    pub track_key: String,
    pub renderer: Arc<dyn ViewerLaneRenderer>,
}

impl fmt::Debug for DecoderTableColumnPresentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecoderTableColumnPresentation")
            .field("source_key", &self.source_key)
            .field("column_key", &self.column_key)
            .field("label", &self.label)
            .field("order", &self.order)
            .field("row_anchor", &self.row_anchor)
            .field("cell_mode", &self.cell_mode)
            .field("track_key", &self.track_key)
            .finish_non_exhaustive()
    }
}

impl DecoderTableColumnPresentation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_key: impl Into<String>,
        column_key: impl Into<String>,
        label: impl Into<String>,
        order: usize,
        row_anchor: bool,
        cell_mode: DecoderTableCellMode,
        track_key: impl Into<String>,
        renderer: Arc<dyn ViewerLaneRenderer>,
    ) -> Self {
        Self {
            source_key: source_key.into(),
            column_key: column_key.into(),
            label: label.into(),
            order,
            row_anchor,
            cell_mode,
            track_key: track_key.into(),
            renderer,
        }
    }
}
