//! Graph → Pipeline compiler (`docs/APP_DESIGN.md`).
//!
//! Two stages: `lower()` turns the UI graph into a pure, diffable
//! `CompiledGraph` IR (prune to sink-reachable nodes, follow reroutes,
//! validate, negotiate per-edge stream kinds); `start_live()` materializes
//! it into a running [`LiveRun`], the supervisor-driven live path used
//! by both the app and its own tests — nothing builds an offline `Pipeline`
//! from this IR anymore; that's what `examples/*.rs` do directly against
//! `signal_processing::Pipeline` for headless/scripted captures.
//!
//! Kind negotiation: each edge picks `offered ∩ accepted`, producer
//! preference order winning. That is what maps one UI `Signal` socket onto
//! the source's dual `d{i}`/`b{i}` ports; every `Words` socket carries the
//! same `Word` runtime type regardless of which decoder produced it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Color32, Pos2};
use serde_json::Value;

use logic_analyzer_viewer::{
    SamplingEdge, SamplingOverlay, SamplingQualifier, ViewerOutputPresentation,
    WaveformPresentationRegistry,
};
use node_graph::{
    Connection, GraphState, Node, NodeId, NodeKind, Socket, SocketDirection, SocketId, SocketShape,
    VariadicInfo,
};
use signal_processing::{
    AcquisitionContext, AcquisitionError, AcquisitionResult, AppManager, CaptureChannelId,
    CaptureIndexFactory, CaptureProviderCapabilities, CaptureSessionPlan, CaptureStartMode,
    CaptureStoreCursor, CollectedPayloadAdapter, CollectedPayloadRegistrationError,
    CollectedPayloadRegistry, ConfigurationBoundary, DerivedDataRetention, DerivedLanes,
    DisconnectEvent, InputSub, NodeConfig, OverflowPolicy, PersistentStoreConfig,
    PreparedAcquisition, ProcessNode, SampleBlock, SamplingActivity, SimpleTriggerCondition,
    TriggerEditorSchema, TriggerProgram,
};

use super::cache_platform;
use super::data_collector::DataCollectorBuilder;
use super::errors::{ApplyError, CompileError};
use super::port_kind::{PortKind, PortValue};
use crate::decoder_table::{DecoderTableColumnPresentation, DecoderTableRegistry};

/// Shared resources handed to builders. A fresh `DerivedLanes` store per
/// run makes stale collected data vanish atomically on re-run.
#[derive(Default)]
pub struct CompileCtx {
    derived_lanes: DerivedLanes,
    waveform_presentations: WaveformPresentationRegistry,
    decoder_tables: DecoderTableRegistry,
    /// Storage policy selected by the graph's source. Finite sources retain
    /// their complete timeline; continuous sources can explicitly choose a
    /// bounded rolling window.
    derived_data_retention: DerivedDataRetention,
    derived_word_caches: Vec<Option<PersistentStoreConfig>>,
    persistent_cache_directory: Option<std::path::PathBuf>,
    /// Clocked-node sampling overlays resolved during lowering. The host
    /// application chooses at most one candidate to display.
    sampling_overlays: Vec<SamplingOverlayCandidate>,
    sampling_activities: HashMap<(String, usize), SamplingActivity>,
}

impl CompileCtx {
    pub fn derived_lanes(&self) -> &DerivedLanes {
        &self.derived_lanes
    }

    pub fn waveform_presentations(&self) -> &WaveformPresentationRegistry {
        &self.waveform_presentations
    }

    pub fn decoder_tables(&self) -> &DecoderTableRegistry {
        &self.decoder_tables
    }

    pub fn derived_data_retention(&self) -> DerivedDataRetention {
        self.derived_data_retention
    }

    pub fn derived_word_cache(&self, member: usize) -> Option<&PersistentStoreConfig> {
        self.derived_word_caches
            .get(member)
            .and_then(Option::as_ref)
    }

    pub fn set_persistent_cache_directory(&mut self, directory: std::path::PathBuf) {
        self.persistent_cache_directory = Some(directory);
    }

    pub fn take_sampling_overlays(&mut self) -> Vec<SamplingOverlayCandidate> {
        std::mem::take(&mut self.sampling_overlays)
    }

    pub fn sampling_activity(&self, runtime_name: &str, input: usize) -> Option<SamplingActivity> {
        self.sampling_activities
            .get(&(runtime_name.to_owned(), input))
            .cloned()
    }
}

/// Input-definition references supplied by a concrete clocked builder. The
/// compiler resolves these references without interpreting socket names or
/// protocol semantics.
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
    /// Whether the concrete runtime can publish this input's activity when
    /// it is not backed directly by a displayed capture channel.
    pub runtime_fallback: bool,
}

/// A fully resolved, selectable sampling overlay belonging to one graph node.
#[derive(Debug, Clone)]
pub struct SamplingOverlayCandidate {
    node_id: NodeId,
    node_title: String,
    overlay: SamplingOverlay,
    runtime_activities: Vec<(usize, SamplingActivity)>,
}

impl SamplingOverlayCandidate {
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn node_title(&self) -> &str {
        &self.node_title
    }

    pub fn overlay(&self) -> &SamplingOverlay {
        &self.overlay
    }
}

/// What one input edge settled on: the negotiated stream kind plus a
/// human-readable producer label (`"{node title}.{socket}"`, used for
/// retained derived-lane identities).
#[derive(Debug, Clone)]
pub struct ResolvedInput {
    pub kind: PortKind,
    pub source: String,
    pub source_node: NodeId,
    pub source_node_title: String,
    pub word_display_format: Option<String>,
    pub viewer_presentation: Option<ViewerOutputPresentation>,
    pub decoder_table_column: Option<DecoderTableColumnPresentation>,
    /// Displayed capture channel from which this edge originates. Concrete
    /// source builders provide it explicitly; generic lowering never parses
    /// runtime port names or display labels.
    pub capture_channel: Option<usize>,
}

/// Per input socket, keyed `(def_index, member_index)`. Keys are
/// def-relative so variadic growth does not shift them.
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
    /// Members of a variadic group in port order.
    pub fn members(&self, def_index: usize) -> Vec<(usize, &ResolvedInput)> {
        let mut members: Vec<(usize, &ResolvedInput)> = self
            .0
            .iter()
            .filter(|((def, _), _)| *def == def_index)
            .map(|((_, member), input)| (*member, input))
            .collect();
        members.sort_by_key(|(member, _)| *member);
        members
    }
}

// ── Builder trait & registry ─────────────────────────────────────────────────

/// Reusable concrete lowering contract for one captured source.
///
/// The feature creates this factory before handing provider ownership to the
/// acquisition worker. It therefore preserves the captured channel-to-port
/// mapping and timebase without retaining or rediscovering the provider.
pub trait CaptureGraphSourceFactory: Send + Sync {
    fn create(&self, cursor: Box<dyn CaptureStoreCursor>) -> Result<Box<dyn ProcessNode>, String>;
}

/// Portable simple-trigger presentation and edit identity for one captured input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleTriggerChannel {
    pub channel_id: CaptureChannelId,
    pub viewer_channel: usize,
    pub name: String,
    pub enabled: bool,
    pub condition: SimpleTriggerCondition,
}

/// Pure graph-state trigger configuration exposed independently of acquisition availability.
///
/// This contract is available on native and wasm targets. It contains only opaque channel
/// identities, generic schema metadata, and the neutral saved program.
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
        let all_channel_ids: Vec<_> = channels
            .iter()
            .map(|channel| channel.channel_id.clone())
            .collect();
        let channel_ids: Vec<_> = channels
            .iter()
            .filter(|channel| channel.enabled)
            .map(|channel| channel.channel_id.clone())
            .collect();
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
pub struct DiscoveredTriggerConfiguration {
    pub source_node: NodeId,
    pub source_title: String,
    pub feature: TriggerConfigurationFeature,
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

/// State-bound live-acquisition capability supplied by a concrete graph node.
///
/// The compiler and application treat channel identities as opaque and do not
/// know which provider, transport, or protocol implements this feature.
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

    /// Captures the concrete runtime port mapping and timebase independently
    /// of provider ownership. The same factory creates the live-following
    /// source and every finalized-session replay source.
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

pub struct DiscoveredLiveCaptureFeature {
    source_node: NodeId,
    source_title: String,
    feature: Box<dyn LiveCaptureFeature>,
}

impl DiscoveredLiveCaptureFeature {
    pub fn new(
        source_node: NodeId,
        source_title: impl Into<String>,
        feature: Box<dyn LiveCaptureFeature>,
    ) -> Self {
        Self {
            source_node,
            source_title: source_title.into(),
            feature,
        }
    }

    pub fn channels(&self) -> &[CaptureChannelId] {
        self.feature.channels()
    }

    pub fn source_node(&self) -> NodeId {
        self.source_node
    }

    pub fn source_title(&self) -> &str {
        &self.source_title
    }

    pub fn channel_names(&self) -> &[String] {
        self.feature.channel_names()
    }

    pub fn sample_rate_hz(&self) -> f64 {
        self.feature.sample_rate_hz()
    }

    pub fn capabilities(&self) -> &CaptureProviderCapabilities {
        self.feature.capabilities()
    }

    pub fn simple_trigger_channels(&self) -> &[SimpleTriggerChannel] {
        self.feature.simple_trigger_channels()
    }

    pub fn trigger_program(&self) -> Option<&TriggerProgram> {
        self.feature.trigger_program()
    }

    pub fn has_trigger_program(&self) -> bool {
        self.trigger_program().is_some() || self.has_simple_trigger()
    }

    pub fn session_plan(&self) -> Option<&CaptureSessionPlan> {
        self.feature.session_plan()
    }

    pub fn has_simple_trigger(&self) -> bool {
        self.simple_trigger_channels()
            .iter()
            .any(|channel| channel.enabled && channel.condition != SimpleTriggerCondition::Ignore)
    }

    pub fn graph_source_factory(&self) -> Arc<dyn CaptureGraphSourceFactory> {
        self.feature.graph_source_factory()
    }

    pub fn prepare(
        self,
        context: AcquisitionContext,
        mode: CaptureStartMode,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        self.feature.prepare_with_mode(context, mode)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCaptureDiscoveryError {
    pub source_nodes: Vec<NodeId>,
    pub message: String,
}

pub trait RuntimeBuilder {
    /// Produces the graph's time domain (exactly one per graph).
    fn is_source(&self) -> bool {
        false
    }
    /// Retention policy for exact derived-data entries in this source's time
    /// domain. Summaries remain complete under bounded retention.
    fn derived_data_retention(&self, _state: &Value) -> DerivedDataRetention {
        DerivedDataRetention::Unlimited
    }
    /// Terminal consumer; pruning keeps only nodes reachable from sinks.
    fn is_sink(&self) -> bool {
        false
    }
    /// Declares a graph-level subscription to retained data without owning a
    /// runtime consumer. Lowering materializes a neutral collector for it.
    fn is_data_subscription(&self) -> bool {
        false
    }
    /// Retains typed output streams independently of any presentation subscriber.
    fn is_data_collector(&self) -> bool {
        false
    }
    /// Stable retained-lane identities published by this collector.
    fn collected_lane_names(
        &self,
        _state: &Value,
        _resolved: &ResolvedInputs,
    ) -> Vec<(usize, String)> {
        Vec::new()
    }
    /// Registers presentation subscribers for retained lane identities.
    /// Collection has already been planned and is independent of this hook.
    fn register_presentations(
        &self,
        _name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _lane_names: &[(usize, String)],
        _ctx: &CompileCtx,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Kinds this input socket can consume, in no particular order.
    fn accepted_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind>;
    /// Kinds this output socket can produce, in preference order.
    fn offered_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind>;
    /// Runtime port name once the edge kind is fixed. `member_index` numbers
    /// variadic group members (D 1 → 0, D 2 → 1, …).
    fn input_port(
        &self,
        socket: &Socket,
        member_index: usize,
        state: &Value,
        kind: PortKind,
    ) -> Option<String>;
    fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String>;
    /// Optional display metadata for a decoded-word output. Kept generic so
    /// the compiler never needs to identify a concrete decoder.
    fn word_display_format(&self, _socket: &Socket, _state: &Value) -> Option<String> {
        None
    }
    /// Optional protocol-neutral presentation contract for this output when
    /// it has a waveform subscription. Generic lowering carries the value
    /// opaquely; concrete producer builders own its semantics.
    fn viewer_output_presentation(
        &self,
        _socket: &Socket,
        _state: &Value,
    ) -> Option<ViewerOutputPresentation> {
        None
    }
    /// Optional protocol-neutral table column for retained output data.
    fn decoder_table_column(
        &self,
        _socket: &Socket,
        _state: &Value,
    ) -> Option<DecoderTableColumnPresentation> {
        None
    }
    /// Raw capture channel represented by this output, when it corresponds
    /// directly to a channel displayed by the logic-analyzer viewer.
    fn viewer_channel_origin(&self, _socket: &Socket, _state: &Value) -> Option<usize> {
        None
    }
    /// Optional pre-run raw-capture presentation supplied by this concrete source.
    fn capture_presentation(&self, _state: &Value) -> Result<Option<CapturePresentation>, String> {
        Ok(None)
    }
    /// Opaque identity for a finite capture source's raw data. A dynamic
    /// source cannot safely reuse persistent derived data before its input is
    /// known at runtime.
    fn capture_cache_identity(
        &self,
        _state: &Value,
        _resolved: &ResolvedInputs,
    ) -> CaptureCacheIdentity {
        CaptureCacheIdentity::NotCapture
    }
    /// Optional protocol-neutral description of how this node samples its
    /// inputs. Concrete builders own the mapping from node state and input
    /// definitions to this electrical presentation contract.
    fn sampling_overlay(&self, _state: &Value) -> Option<SamplingOverlayDescriptor> {
        None
    }
    /// Optional state-bound live acquisition exposed by this concrete node.
    /// Implementations parse their own state and return generic capability
    /// metadata plus a provider preparation boundary.
    fn live_capture_feature(
        &self,
        _state: &Value,
    ) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
        Ok(None)
    }
    /// Optional trigger configuration owned by this node's serialized state.
    ///
    /// Unlike `live_capture_feature`, this pure-data contract must not require a device, native
    /// backend, or acquisition preparation and can therefore remain available on wasm.
    fn trigger_configuration(
        &self,
        _state: &Value,
    ) -> Result<Option<TriggerConfigurationFeature>, String> {
        Ok(None)
    }
    /// Applies a portable feature edit to this node's serialized state. Concrete builders own
    /// channel identity and state evolution; the compiler only routes the opaque request.
    fn apply_live_capture_edit(
        &self,
        _state: &Value,
        _edit: &LiveCaptureEdit,
    ) -> Result<Option<Value>, String> {
        Ok(None)
    }
    /// Whether an unconnected input is a compile error (given the state:
    /// e.g. CS is only required while its polarity isn't Disabled).
    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        true
    }
    /// Overrides the policy-table buffer size (`docs/APP_DESIGN.md`) for this input's
    /// incoming edge. `None` (default, every built-in node) keeps today's
    /// `PortKind`-based sizing. Only a node whose buffer size is a
    /// user-visible property (the `Buffer` node) needs this.
    fn input_buffer_override(&self, _socket: &Socket, _state: &Value) -> Option<usize> {
        None
    }
    /// Instantiate the runtime node. `name` is the pipeline node name (used
    /// for thread naming/logs); `resolved` carries each input's kind so
    /// polymorphic consumers pick the matching concrete type.
    fn build(
        &self,
        _name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Err("graph-only builder has no runtime node".to_owned())
    }

    /// Runtime configuration for a *hot* state change, if this node type can
    /// apply the whole state without restarting (a hot prop change).
    /// `None` (default) means a state change restarts the node in place.
    fn hot_config(&self, _state: &Value) -> Option<NodeConfig> {
        None
    }
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

pub struct DiscoveredCapturePresentation {
    pub identity: String,
    pub presentation: CapturePresentation,
}

pub struct BuilderRegistry {
    builders: HashMap<String, Box<dyn RuntimeBuilder>>,
    collected_payloads: CollectedPayloadRegistry,
}

impl BuilderRegistry {
    pub fn standard() -> Self {
        let mut registry = Self {
            builders: crate::nodes::standard_builders(),
            collected_payloads: CollectedPayloadRegistry::new(),
        };
        signal_processing::register_builtin_collected_payload_adapters(
            &mut registry.collected_payloads,
        )
        .expect("built-in collected payload adapters must be valid");
        registry
    }

    /// Adds (or overwrites) one builder, keyed the same way `standard()`
    /// keys its own entries — the string must match the corresponding
    /// `NodeDef::name()`. Lets a plugin crate extend the registry `standard()`
    /// builds, without touching `standard()` itself.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        builder: Box<dyn RuntimeBuilder>,
    ) -> &mut Self {
        self.builders.insert(name.into(), builder);
        self
    }

    /// Registers a payload that has explicit retained-data semantics.
    ///
    /// This also registers the payload with the generic runtime channel
    /// factory. The present registry records only the durable identity; a
    /// later adapter registration supplies its typed ingestion and query
    /// behavior.
    pub fn register_collected_payload<T: PortValue>(
        &mut self,
        stable_id: impl Into<String>,
    ) -> Result<&mut Self, CollectedPayloadRegistrationError> {
        signal_processing::register_type::<T>();
        self.collected_payloads.register::<T>(stable_id)?;
        Ok(self)
    }

    /// Registers a typed retained-data adapter for a payload identity.
    pub fn register_collected_payload_adapter<T: PortValue>(
        &mut self,
        stable_id: impl Into<String>,
        adapter: std::sync::Arc<dyn CollectedPayloadAdapter>,
    ) -> Result<&mut Self, CollectedPayloadRegistrationError> {
        self.register_collected_payload::<T>(stable_id)?;
        self.collected_payloads.register_adapter::<T>(adapter)?;
        Ok(self)
    }

    /// Registered retained-payload identities, keyed by runtime `TypeId` and
    /// durable plugin-owned identifiers.
    pub fn collected_payloads(&self) -> &CollectedPayloadRegistry {
        &self.collected_payloads
    }

    pub(crate) fn get(&self, def_name: &str) -> Option<&dyn RuntimeBuilder> {
        self.builders.get(def_name).map(|b| b.as_ref())
    }
}

/// Discovers a concrete source's pre-run presentation through its builder contract.
pub fn discover_capture_presentation(
    graph: &GraphState,
    builders: &BuilderRegistry,
) -> Result<Option<DiscoveredCapturePresentation>, String> {
    let mut candidates = Vec::new();
    for (&node_id, node) in &graph.nodes {
        if node.kind != NodeKind::Regular || node.muted {
            continue;
        }
        let Some(builder) = builders.get(node.def_name()) else {
            continue;
        };
        let Some(presentation) = builder.capture_presentation(&node.state)? else {
            continue;
        };
        let state = serde_json::to_vec(&node.state).map_err(|error| error.to_string())?;
        candidates.push(DiscoveredCapturePresentation {
            identity: format!("{node_id:?}:{}", blake3::hash(&state).to_hex()),
            presentation,
        });
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        count => Err(format!(
            "the graph has {count} enabled sources with pre-run capture presentations"
        )),
    }
}

/// Resolves exactly one enabled live-capture feature without identifying a
/// concrete node type. Muted nodes do not participate in acquisition.
pub fn discover_live_capture_feature(
    graph: &GraphState,
    builders: &BuilderRegistry,
) -> Result<Option<DiscoveredLiveCaptureFeature>, LiveCaptureDiscoveryError> {
    discover_live_capture_feature_from(graph, builders, |_| true)
}

/// Resolves exactly one enabled trigger-configuration feature without consulting acquisition
/// backends or identifying a concrete node type.
pub fn discover_trigger_configuration(
    graph: &GraphState,
    builders: &BuilderRegistry,
) -> Result<Option<DiscoveredTriggerConfiguration>, LiveCaptureDiscoveryError> {
    let mut candidates = Vec::new();
    for node in graph
        .nodes
        .values()
        .filter(|node| node.kind == NodeKind::Regular && !node.muted)
    {
        let Some(builder) = builders.get(node.def_name()) else {
            continue;
        };
        match builder.trigger_configuration(&node.state) {
            Ok(Some(feature)) => candidates.push(DiscoveredTriggerConfiguration {
                source_node: node.id,
                source_title: node.title.clone(),
                feature,
            }),
            Ok(None) => {}
            Err(message) => {
                return Err(LiveCaptureDiscoveryError {
                    source_nodes: vec![node.id],
                    message: format!("{}: {message}", node.title),
                });
            }
        }
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => Err(LiveCaptureDiscoveryError {
            source_nodes: candidates
                .iter()
                .map(|candidate| candidate.source_node)
                .collect(),
            message: "multiple enabled trigger configurations are present; keep one capture source enabled"
                .into(),
        }),
    }
}

/// Routes a portable live-feature edit to the concrete builder that owns `source_node`.
pub fn apply_live_capture_edit(
    graph: &GraphState,
    builders: &BuilderRegistry,
    source_node: NodeId,
    edit: &LiveCaptureEdit,
) -> Result<Value, String> {
    let node = graph
        .nodes
        .get(&source_node)
        .ok_or_else(|| format!("live capture source {source_node:?} no longer exists"))?;
    let builder = builders
        .get(node.def_name())
        .ok_or_else(|| format!("no runtime builder is registered for {}", node.def_name()))?;
    builder
        .apply_live_capture_edit(&node.state, edit)?
        .ok_or_else(|| format!("{} does not support this live capture edit", node.title))
}

/// Resolves a live feature only from nodes retained by a successfully
/// compiled graph. This prevents a disconnected development or hardware node
/// from becoming the acquisition source for a different active time domain.
fn discover_live_capture_feature_from(
    graph: &GraphState,
    builders: &BuilderRegistry,
    include: impl Fn(&Node) -> bool,
) -> Result<Option<DiscoveredLiveCaptureFeature>, LiveCaptureDiscoveryError> {
    let mut candidates = Vec::new();
    for node in graph
        .nodes
        .values()
        .filter(|node| node.kind == NodeKind::Regular && !node.muted && include(node))
    {
        let Some(builder) = builders.get(node.def_name()) else {
            continue;
        };
        match builder.live_capture_feature(&node.state) {
            Ok(Some(feature)) => {
                let trigger_channels = feature.simple_trigger_channels();
                let trigger_ids: HashSet<_> = trigger_channels
                    .iter()
                    .map(|channel| &channel.channel_id)
                    .collect();
                let trigger_viewer_channels: HashSet<_> = trigger_channels
                    .iter()
                    .map(|channel| channel.viewer_channel)
                    .collect();
                let duplicate_trigger_channels = trigger_ids.len() != trigger_channels.len()
                    || trigger_viewer_channels.len() != trigger_channels.len();
                let invalid = if feature.channels().is_empty() {
                    Some("live capture exposes no channels")
                } else if feature.channel_names().len() != feature.channels().len() {
                    Some("live capture channel names do not match its channel table")
                } else if !feature.sample_rate_hz().is_finite() || feature.sample_rate_hz() <= 0.0 {
                    Some("live capture sample rate must be positive")
                } else if !feature
                    .capabilities()
                    .supports(feature.channels(), feature.sample_rate_hz())
                {
                    Some("live capture settings are not advertised by the provider")
                } else if feature
                    .simple_trigger_channels()
                    .iter()
                    .any(|channel| channel.viewer_channel >= feature.channels().len())
                {
                    Some("live capture trigger channel references an unknown viewer channel")
                } else if feature.session_plan().is_some_and(|plan| {
                    plan.channel_count != feature.channels().len()
                        || plan.sample_rate_hz as f64 != feature.sample_rate_hz()
                }) {
                    Some("live capture session plan differs from its active channel/rate tuple")
                } else if feature.session_plan().is_some_and(|plan| {
                    plan.policy
                        .effective
                        .trigger_timeout
                        .is_some_and(|timeout| {
                            timeout.action == signal_processing::TriggerTimeoutAction::ForceTrigger
                                && !feature.capabilities().commands().force_trigger
                        })
                }) {
                    Some("live capture policy requests Force Trigger without advertising it")
                } else if duplicate_trigger_channels {
                    Some("live capture trigger channels must have unique identities and lanes")
                } else {
                    None
                };
                if let Some(message) = invalid {
                    return Err(LiveCaptureDiscoveryError {
                        source_nodes: vec![node.id],
                        message: format!("{}: {message}", node.title),
                    });
                }
                candidates.push(DiscoveredLiveCaptureFeature::new(
                    node.id,
                    node.title.clone(),
                    feature,
                ));
            }
            Ok(None) => {}
            Err(message) => {
                return Err(LiveCaptureDiscoveryError {
                    source_nodes: vec![node.id],
                    message: format!("{}: {message}", node.title),
                });
            }
        }
    }

    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => {
            let mut source_nodes: Vec<_> = candidates
                .iter()
                .map(|candidate| candidate.source_node)
                .collect();
            source_nodes.sort_unstable_by_key(|node| node.0);
            Err(LiveCaptureDiscoveryError {
                source_nodes,
                message: "the graph contains multiple live capture sources".into(),
            })
        }
    }
}

pub(crate) fn parse_state<T: serde::de::DeserializeOwned>(state: &Value) -> Result<T, String> {
    serde_json::from_value(state.clone()).map_err(|e| format!("invalid node state: {e}"))
}

// ── IR ───────────────────────────────────────────────────────────────────────

/// Pure description — no threads, no channels. Cheap to rebuild on every
/// edit and cheap to diff (live reconfiguration).
#[derive(Debug, Clone, Default)]
pub struct CompiledGraph {
    pub nodes: Vec<CompiledNode>,
    pub edges: Vec<CompiledEdge>,
    pub derived_data_retention: DerivedDataRetention,
    pub sampling_overlays: Vec<SamplingOverlayCandidate>,
}

#[derive(Debug, Clone)]
pub struct CompiledNode {
    pub id: NodeId,
    /// `BuilderRegistry` key (the UI def name).
    pub builder: String,
    pub state: Value,
    /// Pipeline node name: `n{id}_{title_slug}`.
    pub runtime_name: String,
    pub data_collector: bool,
    pub resolved: ResolvedInputs,
    pub capture_cache_identity: CaptureCacheIdentity,
    pub derived_word_caches: Vec<Option<PersistentStoreConfig>>,
}

#[derive(Debug, Clone)]
pub struct CompiledEdge {
    pub from: (NodeId, String),
    pub to: (NodeId, String),
    pub buffer: usize,
    pub kind: PortKind,
}

fn runtime_name(node: &Node) -> String {
    let slug: String = node
        .title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("n{}_{}", node.id.0, slug.trim_matches('_'))
}

// ── Stage 1: lower ───────────────────────────────────────────────────────────

/// A UI wire with reroutes and muted nodes collapsed away: both endpoints on
/// live (non-reroute, non-muted) regular nodes.
struct Wire {
    from: SocketId,
    to: SocketId,
}

enum WireSource {
    Found(SocketId),
    /// Upstream is missing entirely (e.g. an unplugged reroute) — silently
    /// drop the wire, matching pre-existing reroute behavior.
    Dangling,
    /// A muted node's output has no viable pass-through: either its own
    /// sockets have no type-compatible input at all (a type-transforming
    /// node like a decoder, or a source with nothing to pass through in the
    /// first place), or the one that would match isn't connected.
    MutedBlocked {
        output: SocketId,
    },
}

/// Chases `from` back through any run of Reroute and muted nodes to the
/// effective producing socket. At a muted hop, follows
/// `Node::mute_pass_through_pairs` — the node's own declared input/output
/// type pairing, independent of whatever is wired downstream (mirrors
/// Blender: a muted node only usefully bypasses through a same-typed
/// input/output pair; a type-transforming node has none, so its output has
/// nothing to splice to and just drops).
fn resolve_wire_source(graph: &GraphState, from: SocketId, hops: &mut usize) -> WireSource {
    *hops += 1;
    if *hops > graph.connections.len() + graph.nodes.len() + 1 {
        return WireSource::Dangling; // cycle guard
    }
    let Some(node) = graph.nodes.get(&from.node) else {
        return WireSource::Dangling;
    };
    if node.kind == NodeKind::Reroute {
        return match graph.connections.iter().find(|c| c.to.node == from.node) {
            Some(upstream) => resolve_wire_source(graph, upstream.from, hops),
            None => WireSource::Dangling,
        };
    }
    if node.muted {
        let Some(&(_, in_idx)) = node
            .mute_pass_through_pairs()
            .iter()
            .find(|(out_idx, _)| *out_idx == from.index)
        else {
            return WireSource::MutedBlocked { output: from };
        };
        let paired_input = SocketId {
            node: from.node,
            index: in_idx,
            direction: SocketDirection::Input,
        };
        return match graph.connections.iter().find(|c| c.to == paired_input) {
            Some(upstream) => resolve_wire_source(graph, upstream.from, hops),
            None => WireSource::MutedBlocked { output: from },
        };
    }
    WireSource::Found(from)
}

fn resolve_reroute_edges(graph: &GraphState) -> (Vec<Wire>, Vec<CompileError>) {
    let mut wires = Vec::new();
    let mut errors = Vec::new();
    let mut blocked: HashSet<SocketId> = HashSet::new();
    for connection in &graph.connections {
        let Some(to_node) = graph.nodes.get(&connection.to.node) else {
            continue;
        };
        if to_node.kind == NodeKind::Reroute || to_node.muted {
            // Handled when the wire *leaving* it is resolved.
            continue;
        }
        let mut hops = 0usize;
        match resolve_wire_source(graph, connection.from, &mut hops) {
            WireSource::Found(from) => wires.push(Wire {
                from,
                to: connection.to,
            }),
            WireSource::Dangling => {}
            WireSource::MutedBlocked { output } => {
                if blocked.insert(output) {
                    let output_name = graph
                        .nodes
                        .get(&output.node)
                        .and_then(|n| n.outputs.get(output.index))
                        .map(|s| s.name.as_str())
                        .unwrap_or("?");
                    let to_label = graph
                        .nodes
                        .get(&connection.to.node)
                        .and_then(|n| n.inputs.get(connection.to.index).map(|s| (n, s)))
                        .map(|(n, s)| format!("{}.{}", n.title, s.name))
                        .unwrap_or_else(|| "?".to_string());
                    errors.push(CompileError::on(
                        output.node,
                        format!(
                            "Muted: '{output_name}' has no type-matching input to pass through — '{to_label}' loses its input"
                        ),
                    ));
                }
            }
        }
    }
    (wires, errors)
}

/// Position of a variadic member within its group (0-based); 0 for plain
/// sockets.
fn member_index(node: &Node, socket_index: usize) -> usize {
    let Some(socket) = node.inputs.get(socket_index) else {
        return 0;
    };
    if !socket.is_variadic_member() {
        return 0;
    }
    node.inputs[..socket_index]
        .iter()
        .filter(|other| other.def_index == socket.def_index && other.is_variadic_member())
        .count()
}

/// Fixed id for the compiler-synthesized `Viewer` sink that gathers every
/// selectable output checked in the graph widget's generic View panel
/// (`Socket::view_selectable`, `Socket::show_in_view`, `docs/APP_DESIGN.md`)
/// without an explicit wire.
/// Kept constant (rather than derived from the graph's own ids) so
/// live-diffing sees the same node across `lower()` calls while the watched
/// set is unchanged, regardless of how many real nodes come and go.
const AUTO_VIEW_NODE_ID: NodeId = NodeId(u32::MAX);
const AUTO_DATA_COLLECTOR_NODE_ID: NodeId = NodeId(u32::MAX - 1);

/// If any output in `graph` is checked "Show in view", returns a clone with
/// a synthetic `Viewer` node wired to every one of them — the View panel's
/// checkboxes become lanes without the user dragging a wire. Reuses the
/// exact same pruning and edge-negotiation path an explicit Viewer
/// connection would take, so nothing downstream in `lower()` needs to know
/// this node isn't real.
fn with_auto_view_sink(graph: &GraphState, registry: &BuilderRegistry) -> GraphState {
    let mut watched: Vec<(SocketId, String)> = graph
        .nodes
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::Regular)
        .flat_map(|(&id, node)| {
            node.outputs
                .iter()
                .enumerate()
                .filter(|(_, output)| {
                    output.visible && output.view_selectable && output.show_in_view
                })
                .map(move |(index, output)| {
                    (
                        SocketId {
                            node: id,
                            index,
                            direction: SocketDirection::Output,
                        },
                        format!("{}.{}", node.title, output.name),
                    )
                })
        })
        .collect();
    // Concrete nodes order related presentation outputs in their socket
    // schema. Preserve that explicit order without interpreting labels.
    watched.sort_by_key(|(socket, _)| (socket.node.0, socket.index));

    let mut tabled: Vec<(SocketId, String)> = graph
        .nodes
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::Regular)
        .flat_map(|(&id, node)| {
            let Some(builder) = registry.get(node.def_name()) else {
                return Vec::new().into_iter();
            };
            node.outputs
                .iter()
                .enumerate()
                .filter(|(index, output)| {
                    let collected_for_view =
                        output.visible && output.view_selectable && output.show_in_view;
                    let collected_by_explicit_sink = graph.connections.iter().any(|connection| {
                        connection.from.node == id
                            && connection.from.index == *index
                            && graph
                                .nodes
                                .get(&connection.to.node)
                                .and_then(|target| registry.get(target.def_name()))
                                .is_some_and(|builder| {
                                    builder.is_data_collector() || builder.is_data_subscription()
                                })
                    });
                    !collected_for_view
                        && !collected_by_explicit_sink
                        && builder.decoder_table_column(output, &node.state).is_some()
                        && builder
                            .offered_kinds(output, &node.state)
                            .into_iter()
                            .any(|kind| builder.output_port(output, &node.state, kind).is_some())
                })
                .map(move |(index, output)| {
                    (
                        SocketId {
                            node: id,
                            index,
                            direction: SocketDirection::Output,
                        },
                        format!("{}.{}", node.title, output.name),
                    )
                })
                .collect::<Vec<_>>()
                .into_iter()
        })
        .collect();
    tabled.sort_by_key(|(socket, _)| (socket.node.0, socket.index));

    let mut graph = graph.clone();
    let inputs: Vec<Socket> = watched
        .iter()
        .map(|(_, label)| Socket {
            name: label.clone(),
            type_name: "Signal".to_owned(),
            color: Color32::from_rgb(0, 205, 160),
            shape: SocketShape::Circle,
            allowed: vec![
                "Words".to_owned(),
                "Trigger".to_owned(),
                "Number".to_owned(),
                "Text".to_owned(),
            ],
            resolved_type: None,
            def_index: 0,
            variadic: Some(VariadicInfo {
                base: "In".to_owned(),
                max: watched.len(),
                placeholder: false,
            }),
            visible: true,
            editor_visible: true,
            hidden: false,
            has_control: false,
            view_selectable: false,
            view_indicator_sources: Vec::new(),
            show_in_view: false,
        })
        .collect();
    if !inputs.is_empty() {
        let mut auto_view = Node::blank(AUTO_VIEW_NODE_ID, "Viewer", Pos2::ZERO);
        auto_view.title = "Auto View".to_owned();
        auto_view.header_color = Color32::from_rgb(160, 80, 60);
        auto_view.inputs = inputs;
        auto_view.state = serde_json::json!({ "label": { "value": "" } });
        graph.nodes.insert(AUTO_VIEW_NODE_ID, auto_view);
        graph
            .connections
            .extend(
                watched
                    .into_iter()
                    .enumerate()
                    .map(|(member, (from, _))| Connection {
                        from,
                        to: SocketId {
                            node: AUTO_VIEW_NODE_ID,
                            index: member,
                            direction: SocketDirection::Input,
                        },
                    }),
            );
    }
    if !tabled.is_empty() {
        let inputs = tabled
            .iter()
            .map(|(_, label)| Socket {
                name: label.clone(),
                type_name: "Signal".to_owned(),
                color: Color32::from_rgb(160, 80, 60),
                shape: SocketShape::Circle,
                allowed: vec![
                    "Words".to_owned(),
                    "Trigger".to_owned(),
                    "Number".to_owned(),
                    "Text".to_owned(),
                ],
                resolved_type: None,
                def_index: 0,
                variadic: Some(VariadicInfo {
                    base: "In".to_owned(),
                    max: tabled.len(),
                    placeholder: false,
                }),
                visible: false,
                editor_visible: false,
                hidden: true,
                has_control: false,
                view_selectable: false,
                view_indicator_sources: Vec::new(),
                show_in_view: false,
            })
            .collect();
        let mut collector = Node::blank(
            AUTO_DATA_COLLECTOR_NODE_ID,
            crate::compiler::DATA_COLLECTOR_BUILDER,
            Pos2::ZERO,
        );
        collector.title = "Derived Data Collector".to_owned();
        collector.inputs = inputs;
        collector.state = Value::Null;
        graph.nodes.insert(AUTO_DATA_COLLECTOR_NODE_ID, collector);
        graph
            .connections
            .extend(
                tabled
                    .into_iter()
                    .enumerate()
                    .map(|(member, (from, _))| Connection {
                        from,
                        to: SocketId {
                            node: AUTO_DATA_COLLECTOR_NODE_ID,
                            index: member,
                            direction: SocketDirection::Input,
                        },
                    }),
            );
    }
    graph
}

pub fn lower(
    graph: &GraphState,
    registry: &BuilderRegistry,
) -> Result<CompiledGraph, Vec<CompileError>> {
    let augmented = with_auto_view_sink(graph, registry);
    let graph = &augmented;
    let (wires, mut errors) = resolve_reroute_edges(graph);

    // Prune: keep only what feeds a sink.
    let sinks: Vec<NodeId> = graph
        .nodes
        .values()
        .filter(|node| {
            node.kind == NodeKind::Regular
                && registry
                    .get(node.def_name())
                    .is_some_and(|builder| builder.is_sink() || builder.is_data_subscription())
        })
        .map(|node| node.id)
        .collect();
    if sinks.is_empty() {
        return Err(vec![CompileError::global(
            "Graph has no sink (add a File Writer)",
        )]);
    }
    let mut keep: HashSet<NodeId> = HashSet::new();
    let mut stack = sinks.clone();
    while let Some(id) = stack.pop() {
        if !keep.insert(id) {
            continue;
        }
        for wire in &wires {
            if wire.to.node == id && !keep.contains(&wire.from.node) {
                stack.push(wire.from.node);
            }
        }
    }
    let mut kept: Vec<NodeId> = keep.iter().copied().collect();
    kept.sort_by_key(|id| id.0);

    // Every kept node must have a runtime; exactly one source.
    let mut source_count = 0usize;
    let mut derived_data_retention = DerivedDataRetention::Unlimited;
    for &id in &kept {
        let node = &graph.nodes[&id];
        match registry.get(node.def_name()) {
            None => errors.push(CompileError::on(
                id,
                format!("'{}' has no runtime implementation", node.def_name()),
            )),
            Some(builder) if builder.is_source() => {
                source_count += 1;
                derived_data_retention = builder.derived_data_retention(&node.state);
            }
            Some(_) => {}
        }
    }
    if source_count == 0 {
        errors.push(CompileError::global("Graph has no data source"));
    } else if source_count > 1 {
        for &id in &kept {
            let node = &graph.nodes[&id];
            if registry.get(node.def_name()).is_some_and(|b| b.is_source()) {
                errors.push(CompileError::on(
                    id,
                    "Multiple sources: a graph has exactly one time domain",
                ));
            }
        }
    }

    // Negotiate kinds and ports per edge.
    let mut resolved: HashMap<NodeId, ResolvedInputs> = HashMap::new();
    let mut edges: Vec<CompiledEdge> = Vec::new();
    let mut connected: HashMap<NodeId, HashSet<usize>> = HashMap::new();
    for wire in &wires {
        if !keep.contains(&wire.from.node) || !keep.contains(&wire.to.node) {
            continue;
        }
        let from_node = &graph.nodes[&wire.from.node];
        let to_node = &graph.nodes[&wire.to.node];
        let (Some(from_builder), Some(to_builder)) = (
            registry.get(from_node.def_name()),
            registry.get(to_node.def_name()),
        ) else {
            continue; // already reported above
        };
        let (Some(from_socket), Some(to_socket)) = (
            from_node.outputs.get(wire.from.index),
            to_node.inputs.get(wire.to.index),
        ) else {
            errors.push(CompileError::on(wire.to.node, "Dangling connection"));
            continue;
        };

        connected
            .entry(wire.to.node)
            .or_default()
            .insert(wire.to.index);

        let offered = from_builder.offered_kinds(from_socket, &from_node.state);
        let accepted = to_builder.accepted_kinds(to_socket, &to_node.state);
        let Some(kind) = offered.iter().copied().find(|k| accepted.contains(k)) else {
            errors.push(CompileError::on(
                wire.to.node,
                format!(
                    "'{}' cannot consume what '{}' produces on '{}'",
                    to_socket.name, from_node.title, from_socket.name
                ),
            ));
            continue;
        };

        let Some(out_port) = from_builder.output_port(from_socket, &from_node.state, kind) else {
            errors.push(CompileError::on(
                wire.from.node,
                format!("No runtime port for output '{}'", from_socket.name),
            ));
            continue;
        };
        let member = member_index(to_node, wire.to.index);
        let Some(in_port) = to_builder.input_port(to_socket, member, &to_node.state, kind) else {
            errors.push(CompileError::on(
                wire.to.node,
                format!("No runtime port for input '{}'", to_socket.name),
            ));
            continue;
        };

        resolved.entry(wire.to.node).or_default().0.insert(
            (to_socket.def_index, member),
            ResolvedInput {
                kind,
                source: format!("{}.{}", from_node.title, from_socket.name),
                source_node: wire.from.node,
                source_node_title: from_node.title.clone(),
                word_display_format: from_builder
                    .word_display_format(from_socket, &from_node.state),
                viewer_presentation: from_builder
                    .viewer_output_presentation(from_socket, &from_node.state),
                decoder_table_column: from_builder
                    .decoder_table_column(from_socket, &from_node.state),
                capture_channel: from_builder.viewer_channel_origin(from_socket, &from_node.state),
            },
        );
        edges.push(CompiledEdge {
            from: (wire.from.node, out_port),
            to: (wire.to.node, in_port),
            buffer: to_builder
                .input_buffer_override(to_socket, &to_node.state)
                .unwrap_or_else(|| kind.buffer_size(from_builder.is_source())),
            kind,
        });
    }

    // Required inputs.
    for &id in &kept {
        let node = &graph.nodes[&id];
        let Some(builder) = registry.get(node.def_name()) else {
            continue;
        };
        let node_connected = connected.get(&id);
        for (index, socket) in node.inputs.iter().enumerate() {
            // Control-bearing sockets go through `input_required` like any
            // other: most are self-supplying config (their builders return
            // false), but one can be conditionally required — the writer's
            // Filename picker is required exactly while its value is empty.
            if !socket.visible {
                continue;
            }
            if socket.is_variadic_placeholder() {
                let has_member = node
                    .inputs
                    .iter()
                    .any(|s| s.def_index == socket.def_index && s.is_variadic_member());
                if !has_member && builder.input_required(socket, &node.state) {
                    errors.push(CompileError::on(
                        id,
                        format!("Input '{}' needs at least one connection", socket.name),
                    ));
                }
            } else if !socket.is_variadic_member()
                && !node_connected.is_some_and(|set| set.contains(&index))
                && builder.input_required(socket, &node.state)
            {
                errors.push(CompileError::on(
                    id,
                    format!("Input '{}' is not connected", socket.name),
                ));
            }
        }
    }

    // Cycle check (a cycle would deadlock the pipeline).
    if has_cycle(&kept, &edges) {
        errors.push(CompileError::global("Graph contains a cycle"));
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let nodes: Vec<CompiledNode> = kept
        .iter()
        .map(|&id| {
            let node = &graph.nodes[&id];
            let resolved = resolved.remove(&id).unwrap_or_default();
            let builder = registry
                .get(node.def_name())
                .expect("retained node has a registered builder");
            CompiledNode {
                id,
                builder: node.def_name().to_owned(),
                state: node.state.clone(),
                runtime_name: runtime_name(node),
                data_collector: builder.is_data_collector() || builder.is_data_subscription(),
                capture_cache_identity: builder.capture_cache_identity(&node.state, &resolved),
                resolved,
                derived_word_caches: Vec::new(),
            }
        })
        .collect();
    let sampling_overlays = nodes
        .iter()
        .filter_map(|compiled_node| {
            let builder = registry.get(&compiled_node.builder)?;
            let descriptor = builder.sampling_overlay(&compiled_node.state)?;
            let clock_channel = compiled_node
                .resolved
                .get(descriptor.clock_input, 0)?
                .capture_channel?;
            let mut sampled_channels = descriptor
                .sampled_input_groups
                .iter()
                .flat_map(|def_index| compiled_node.resolved.members(*def_index))
                .filter_map(|(_, input)| input.capture_channel)
                .collect::<Vec<_>>();
            sampled_channels.sort_unstable();
            sampled_channels.dedup();
            if sampled_channels.is_empty() {
                return None;
            }
            let mut qualifiers = Vec::new();
            let mut activities = Vec::new();
            let mut runtime_activities = Vec::new();
            for qualifier in descriptor.qualifiers {
                let Some(input) = compiled_node.resolved.get(qualifier.input, 0) else {
                    continue;
                };
                if let Some(channel) = input.capture_channel {
                    qualifiers.push(SamplingQualifier {
                        channel,
                        active_level: qualifier.active_level,
                    });
                } else if qualifier.runtime_fallback {
                    let activity = SamplingActivity::default();
                    activities.push(activity.clone());
                    runtime_activities.push((qualifier.input, activity));
                } else {
                    return None;
                }
            }
            Some(SamplingOverlayCandidate {
                node_id: compiled_node.id,
                node_title: graph.nodes[&compiled_node.id].title.clone(),
                overlay: SamplingOverlay {
                    clock_channel,
                    sampled_channels,
                    edge: descriptor.edge,
                    qualifiers,
                    activities,
                },
                runtime_activities,
            })
        })
        .collect();
    let compiled = CompiledGraph {
        nodes,
        edges,
        derived_data_retention,
        sampling_overlays,
    };
    let mut compiled = compiled;
    cache_platform::assign_derived_word_caches(&mut compiled);
    Ok(compiled)
}

/// Resolves the clocked-node sampling presentations available for the
/// current graph without starting its runtime. Hosts use this to populate
/// presentation controls before the user runs the pipeline.
pub fn sampling_overlay_candidates(
    graph: &GraphState,
    registry: &BuilderRegistry,
) -> Result<Vec<SamplingOverlayCandidate>, Vec<CompileError>> {
    lower(graph, registry).map(|compiled| compiled.sampling_overlays)
}

fn sampling_activity_map(compiled: &CompiledGraph) -> HashMap<(String, usize), SamplingActivity> {
    compiled
        .sampling_overlays
        .iter()
        .flat_map(|candidate| {
            let runtime_name = compiled_node(compiled, candidate.node_id)
                .runtime_name
                .clone();
            candidate
                .runtime_activities
                .iter()
                .map(move |(input, activity)| ((runtime_name.clone(), *input), activity.clone()))
        })
        .collect()
}

fn reuse_sampling_activities(previous: &CompiledGraph, next: &mut CompiledGraph) {
    for candidate in &mut next.sampling_overlays {
        let Some(previous_candidate) = previous
            .sampling_overlays
            .iter()
            .find(|previous| previous.node_id == candidate.node_id)
        else {
            continue;
        };
        for (input, activity) in &mut candidate.runtime_activities {
            if let Some((_, previous_activity)) = previous_candidate
                .runtime_activities
                .iter()
                .find(|(previous_input, _)| previous_input == input)
            {
                *activity = previous_activity.clone();
            }
        }
        candidate.overlay.activities = candidate
            .runtime_activities
            .iter()
            .map(|(_, activity)| activity.clone())
            .collect();
    }
}

fn has_cycle(nodes: &[NodeId], edges: &[CompiledEdge]) -> bool {
    let mut indegree: HashMap<NodeId, usize> = nodes.iter().map(|&id| (id, 0)).collect();
    for edge in edges {
        *indegree.entry(edge.to.0).or_default() += 1;
    }
    let mut queue: Vec<NodeId> = indegree
        .iter()
        .filter(|entry| *entry.1 == 0)
        .map(|(&id, _)| id)
        .collect();
    let mut visited = 0usize;
    while let Some(id) = queue.pop() {
        visited += 1;
        for edge in edges.iter().filter(|e| e.from.0 == id) {
            let d = indegree.get_mut(&edge.to.0).expect("edge endpoints kept");
            *d -= 1;
            if *d == 0 {
                queue.push(edge.to.0);
            }
        }
    }
    visited != nodes.len()
}

// ── Live pipeline ───────────────────────────────────────────────────────

/// Producers-before-consumers order; `lower` already rejected cycles.
fn topo_order(compiled: &CompiledGraph) -> Vec<NodeId> {
    let mut indegree: HashMap<NodeId, usize> =
        compiled.nodes.iter().map(|node| (node.id, 0)).collect();
    for edge in &compiled.edges {
        *indegree.entry(edge.to.0).or_default() += 1;
    }
    let mut queue: Vec<NodeId> = compiled
        .nodes
        .iter()
        .map(|node| node.id)
        .filter(|id| indegree[id] == 0)
        .collect();
    queue.sort_by_key(|id| id.0);
    let mut order = Vec::with_capacity(compiled.nodes.len());
    while let Some(id) = queue.pop() {
        order.push(id);
        for edge in compiled.edges.iter().filter(|edge| edge.from.0 == id) {
            let degree = indegree.get_mut(&edge.to.0).expect("kept node");
            *degree -= 1;
            if *degree == 0 {
                queue.push(edge.to.0);
            }
        }
    }
    order
}

pub(crate) fn compiled_node(compiled: &CompiledGraph, id: NodeId) -> &CompiledNode {
    compiled
        .nodes
        .iter()
        .find(|node| node.id == id)
        .expect("node in compiled graph")
}

fn materialize_compiled_node(
    node: &CompiledNode,
    builder: &dyn RuntimeBuilder,
    runtime_name: &str,
    collected_payloads: &CollectedPayloadRegistry,
    ctx: &mut CompileCtx,
) -> Result<Box<dyn ProcessNode>, String> {
    if builder.is_data_subscription() {
        return DataCollectorBuilder::build_with_lane_names(
            runtime_name,
            &node.resolved,
            &builder.collected_lane_names(&node.state, &node.resolved),
            collected_payloads,
            ctx,
        );
    }
    builder.build(runtime_name, &node.state, &node.resolved, ctx)
}

fn register_collected_subscribers(
    node: &CompiledNode,
    builder: &dyn RuntimeBuilder,
    subscription_name: &str,
    ctx: &CompileCtx,
) -> Result<(), String> {
    if !node.data_collector {
        return Ok(());
    }
    let lane_names = builder.collected_lane_names(&node.state, &node.resolved);
    crate::decoder_table::subscribe_collected_tables(
        node.id,
        &node.resolved,
        &lane_names,
        ctx.decoder_tables(),
    );
    builder.register_presentations(
        subscription_name,
        &node.state,
        &node.resolved,
        &lane_names,
        ctx,
    )
}

pub fn derived_cache_configs_by_node(
    graph: &GraphState,
    registry: &BuilderRegistry,
    directory: &std::path::Path,
) -> Result<HashMap<NodeId, Vec<PersistentStoreConfig>>, Vec<CompileError>> {
    cache_platform::cache_configs_by_node(graph, registry, directory)
}

/// Input subscriptions for `id`, matched to the built node's input schema.
fn input_subs(
    compiled: &CompiledGraph,
    id: NodeId,
    built: &dyn ProcessNode,
    names: &HashMap<NodeId, String>,
) -> Result<Vec<Option<InputSub>>, String> {
    built
        .input_schema()
        .iter()
        .map(|schema| {
            let edge = compiled
                .edges
                .iter()
                .find(|edge| edge.to.0 == id && edge.to.1 == schema.name);
            match edge {
                None => Ok(None),
                Some(edge) => {
                    let from_node = names
                        .get(&edge.from.0)
                        .ok_or_else(|| format!("producer n{} not materialized", edge.from.0.0))?;
                    Ok(Some(InputSub {
                        from_node: from_node.clone(),
                        from_port: edge.from.1.clone(),
                        buffer: edge.buffer,
                        policy: OverflowPolicy::Block,
                    }))
                }
            }
        })
        .collect()
}

/// One live edit, in application order (removals reverse-topological,
/// additions topological, then hot configs and in-place restarts).
#[derive(Debug)]
enum LiveEdit {
    Remove(NodeId),
    Add(NodeId),
    Configure(NodeId, NodeConfig),
    Restart(NodeId),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ApplySummary {
    pub added: usize,
    pub removed: usize,
    pub configured: usize,
    pub restarted: usize,
}

impl ApplySummary {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Wiring signature of a node's inputs, for diffing.
fn wiring_of(compiled: &CompiledGraph, id: NodeId) -> BTreeSet<(String, u32, String, usize)> {
    compiled
        .edges
        .iter()
        .filter(|edge| edge.to.0 == id)
        .map(|edge| {
            (
                edge.to.1.clone(),
                edge.from.0.0,
                edge.from.1.clone(),
                edge.buffer,
            )
        })
        .collect()
}

/// Classifies the difference between the running IR and the edited one
/// (the edit classes of `docs/APP_DESIGN.md`). Returns the edit list, or
/// the reason a full restart is needed.
fn diff(
    old: &CompiledGraph,
    new: &CompiledGraph,
    registry: &BuilderRegistry,
) -> Result<Vec<LiveEdit>, String> {
    let old_ids: HashSet<NodeId> = old.nodes.iter().map(|node| node.id).collect();
    let new_ids: HashSet<NodeId> = new.nodes.iter().map(|node| node.id).collect();
    let is_source = |compiled: &CompiledGraph, id: NodeId| {
        registry
            .get(&compiled_node(compiled, id).builder)
            .is_some_and(|builder| builder.is_source())
    };

    let mut edits: Vec<LiveEdit> = Vec::new();

    // Removals, consumers before producers.
    let mut removals: Vec<NodeId> = topo_order(old)
        .into_iter()
        .rev()
        .filter(|id| !new_ids.contains(id))
        .collect();
    for &id in &removals {
        if is_source(old, id) {
            return Err("the source node was removed".into());
        }
    }
    edits.extend(removals.drain(..).map(LiveEdit::Remove));

    // Additions, producers before consumers.
    for id in topo_order(new) {
        if old_ids.contains(&id) {
            continue;
        }
        for edge in new.edges.iter().filter(|edge| edge.to.0 == id) {
            if edge.kind == PortKind::of::<SampleBlock>() {
                return Err(
                    "new node consumes block channels; block subscriptions cannot join mid-stream"
                        .to_string(),
                );
            }
            if is_source(new, edge.from.0) {
                return Err(
                    "new connection directly to the source; source destinations are fixed at start"
                        .into(),
                );
            }
        }
        edits.push(LiveEdit::Add(id));
    }

    // Changed nodes: hot config, or restart in place.
    for id in topo_order(new) {
        if !old_ids.contains(&id) {
            continue;
        }
        let old_node = compiled_node(old, id);
        let new_node = compiled_node(new, id);
        let wiring_changed = wiring_of(old, id) != wiring_of(new, id);
        let state_changed = old_node.state != new_node.state;
        if !wiring_changed && !state_changed {
            continue;
        }
        if is_source(new, id) {
            return Err("the source node changed".into());
        }
        let builder = registry
            .get(&new_node.builder)
            .ok_or_else(|| format!("no builder for '{}'", new_node.builder))?;
        if !wiring_changed
            && state_changed
            && let Some(config) = builder.hot_config(&new_node.state)
        {
            edits.push(LiveEdit::Configure(id, config));
            continue;
        }
        // Restart in place: the node re-subscribes to its producers, which
        // is invisible to block streams and to source ports (their worker
        // threads snapshot destinations at start).
        for edge in new.edges.iter().filter(|edge| edge.to.0 == id) {
            if edge.kind == PortKind::of::<SampleBlock>() {
                return Err(format!(
                    "'{}' consumes block channels and cannot restart mid-stream",
                    new_node.runtime_name
                ));
            }
            if is_source(new, edge.from.0) {
                return Err(format!(
                    "'{}' is fed directly by the source and cannot restart mid-stream",
                    new_node.runtime_name
                ));
            }
        }
        edits.push(LiveEdit::Restart(id));
    }

    Ok(edits)
}

/// A pipeline running under the live supervisor: editable while it runs.
pub struct LiveRun {
    manager: AppManager,
    compiled: CompiledGraph,
    /// Supervisor key per UI node — assigned at add time and stable across
    /// title renames and in-place restarts.
    names: HashMap<NodeId, String>,
    lanes: DerivedLanes,
    waveform_presentations: WaveformPresentationRegistry,
    decoder_tables: DecoderTableRegistry,
    /// Set by [`Self::stop`]: the wind-down has been signalled but node
    /// threads may still be finishing their current `work()` call.
    stop_requested: bool,
    cache_pruned: bool,
    persistent_cache_directory: Option<std::path::PathBuf>,
}

/// One provider-owned source process used only while a live capture follows
/// its authoritative store.
pub struct LiveAnalysisSource {
    pub source_node: NodeId,
    pub process: Box<dyn ProcessNode>,
}

/// Explicit source-node replacements used when materializing a graph.
///
/// The compiler validates every node ID against the lowered graph and never
/// interprets the source process or discovers a provider. Live capture and
/// finalized replay therefore share one substitution mechanism.
pub type SourceProcessOverrides = HashMap<NodeId, Box<dyn ProcessNode>>;

/// Lowers and materializes `graph` under an [`AppManager`] — real OS threads
/// natively, a cooperative single-thread runner on wasm.
fn start_live(
    graph: &GraphState,
    registry: &BuilderRegistry,
    ctx: &mut CompileCtx,
) -> Result<LiveRun, Vec<CompileError>> {
    start_live_inner(graph, registry, ctx, SourceProcessOverrides::new())
}

/// Starts the fixed compiled graph with its live-capable source replaced by
/// the process that follows the capture store. All other nodes use the same
/// lowering and materialization path as an ordinary run.
pub fn start_live_analysis(
    graph: &GraphState,
    registry: &BuilderRegistry,
    ctx: &mut CompileCtx,
    source: LiveAnalysisSource,
) -> Result<LiveRun, Vec<CompileError>> {
    let mut overrides = SourceProcessOverrides::new();
    overrides.insert(source.source_node, source.process);
    start_live_inner(graph, registry, ctx, overrides)
}

fn start_live_inner(
    graph: &GraphState,
    registry: &BuilderRegistry,
    ctx: &mut CompileCtx,
    mut source_overrides: SourceProcessOverrides,
) -> Result<LiveRun, Vec<CompileError>> {
    ctx.waveform_presentations().set_implicit_groups(false);
    let mut compiled = lower(graph, registry)?;
    cache_platform::configure_directory(&mut compiled, ctx.persistent_cache_directory.as_deref());
    ctx.derived_data_retention = compiled.derived_data_retention;
    ctx.sampling_overlays
        .clone_from(&compiled.sampling_overlays);
    ctx.sampling_activities = sampling_activity_map(&compiled);
    let mut manager = AppManager::new();
    let mut names: HashMap<NodeId, String> = HashMap::new();

    let (execution, cache_pruned) = cache_platform::prepare_execution(&compiled, registry);

    for node in &execution.nodes {
        let Some(builder) = registry.get(&node.builder) else {
            continue;
        };
        register_collected_subscribers(node, builder, &node.runtime_name, ctx)
            .map_err(|message| vec![CompileError::on(node.id, message)])?;
    }

    for source_node in source_overrides.keys().copied() {
        let Some(node) = execution.nodes.iter().find(|node| node.id == source_node) else {
            return Err(vec![CompileError::on(
                source_node,
                "source override is not retained by the compiled graph",
            )]);
        };
        let is_source = registry
            .get(&node.builder)
            .is_some_and(RuntimeBuilder::is_source);
        if !is_source {
            return Err(vec![CompileError::on(
                source_node,
                "source override does not target a source node",
            )]);
        }
    }

    for id in topo_order(&execution) {
        let node = compiled_node(&execution, id);
        let builder = registry.get(&node.builder).ok_or_else(|| {
            vec![CompileError::on(
                id,
                format!("unknown builder '{}'", node.builder),
            )]
        })?;
        ctx.derived_word_caches
            .clone_from(&node.derived_word_caches);
        let process = if let Some(process) = source_overrides.remove(&id) {
            process
        } else {
            materialize_compiled_node(
                node,
                builder,
                &node.runtime_name,
                registry.collected_payloads(),
                ctx,
            )
            .map_err(|message| vec![CompileError::on(id, message)])?
        };
        let inputs = input_subs(&execution, id, process.as_ref(), &names)
            .map_err(|message| vec![CompileError::on(id, message)])?;
        manager
            .add_node_deferred(signal_processing::NodeSpec {
                name: node.runtime_name.clone(),
                node: process,
                inputs,
            })
            .map_err(|message| vec![CompileError::on(id, message)])?;
        names.insert(id, node.runtime_name.clone());
    }
    // All initial subscriptions exist; only now may threads start (a
    // self-threading source snapshots its subscriber lists on first work()).
    manager
        .start_all_deferred()
        .map_err(|message| vec![CompileError::global(message)])?;

    Ok(LiveRun {
        manager,
        compiled,
        names,
        lanes: ctx.derived_lanes.clone(),
        waveform_presentations: ctx.waveform_presentations.clone(),
        decoder_tables: ctx.decoder_tables.clone(),
        stop_requested: false,
        cache_pruned,
        persistent_cache_directory: ctx.persistent_cache_directory.clone(),
    })
}

impl LiveRun {
    pub fn sampling_overlays(&self) -> &[SamplingOverlayCandidate] {
        &self.compiled.sampling_overlays
    }

    pub fn persistent_cache_configs(&self) -> Vec<PersistentStoreConfig> {
        self.compiled
            .nodes
            .iter()
            .flat_map(|node| node.derived_word_caches.iter().flatten().cloned())
            .collect()
    }

    /// Diffs the edited graph against what is running and applies the
    /// difference live. On any error the running pipeline is untouched
    /// (edits either fail up front in `diff`, or — for build failures midway
    /// — leave already-applied edits in place and report).
    pub fn apply(
        &mut self,
        graph: &GraphState,
        registry: &BuilderRegistry,
    ) -> Result<ApplySummary, ApplyError> {
        let mut new = lower(graph, registry).map_err(ApplyError::Compile)?;
        reuse_sampling_activities(&self.compiled, &mut new);
        cache_platform::configure_directory(&mut new, self.persistent_cache_directory.as_deref());
        let edits = diff(&self.compiled, &new, registry).map_err(ApplyError::NeedsFullRestart)?;
        if edits.is_empty() {
            self.compiled = new;
            return Ok(ApplySummary::default());
        }
        if self.cache_pruned {
            return Err(ApplyError::NeedsFullRestart(
                "the running graph reused persistent derived data; stop and rerun to apply edits"
                    .to_string(),
            ));
        }

        let mut ctx = CompileCtx {
            derived_lanes: self.lanes.clone(),
            waveform_presentations: self.waveform_presentations.clone(),
            decoder_tables: self.decoder_tables.clone(),
            derived_data_retention: new.derived_data_retention,
            derived_word_caches: Vec::new(),
            persistent_cache_directory: self.persistent_cache_directory.clone(),
            sampling_overlays: new.sampling_overlays.clone(),
            sampling_activities: sampling_activity_map(&new),
        };
        let mut summary = ApplySummary::default();
        for edit in edits {
            match edit {
                LiveEdit::Remove(id) => {
                    if let Some(name) = self.names.remove(&id) {
                        self.manager.remove_node(&name).map_err(ApplyError::Apply)?;
                    }
                    summary.removed += 1;
                }
                LiveEdit::Add(id) => {
                    let node = compiled_node(&new, id);
                    let builder = registry.get(&node.builder).ok_or_else(|| {
                        ApplyError::Apply(format!("no builder '{}'", node.builder))
                    })?;
                    ctx.derived_word_caches
                        .clone_from(&node.derived_word_caches);
                    register_collected_subscribers(node, builder, &node.runtime_name, &ctx)
                        .map_err(ApplyError::Apply)?;
                    let process = materialize_compiled_node(
                        node,
                        builder,
                        &node.runtime_name,
                        registry.collected_payloads(),
                        &mut ctx,
                    )
                    .map_err(ApplyError::Apply)?;
                    let inputs = input_subs(&new, id, process.as_ref(), &self.names)
                        .map_err(ApplyError::Apply)?;
                    self.manager
                        .add_node(signal_processing::NodeSpec {
                            name: node.runtime_name.clone(),
                            node: process,
                            inputs,
                        })
                        .map_err(ApplyError::Apply)?;
                    self.names.insert(id, node.runtime_name.clone());
                    summary.added += 1;
                }
                LiveEdit::Configure(id, config) => {
                    let name = self
                        .names
                        .get(&id)
                        .ok_or_else(|| ApplyError::Apply(format!("n{} not running", id.0)))?;
                    self.manager
                        .reconfigure(name, config)
                        .map_err(ApplyError::Apply)?;
                    summary.configured += 1;
                }
                LiveEdit::Restart(id) => {
                    let node = compiled_node(&new, id);
                    let name = self
                        .names
                        .get(&id)
                        .cloned()
                        .ok_or_else(|| ApplyError::Apply(format!("n{} not running", id.0)))?;
                    let builder = registry.get(&node.builder).ok_or_else(|| {
                        ApplyError::Apply(format!("no builder '{}'", node.builder))
                    })?;
                    ctx.derived_word_caches
                        .clone_from(&node.derived_word_caches);
                    register_collected_subscribers(node, builder, &name, &ctx)
                        .map_err(ApplyError::Apply)?;
                    let process = materialize_compiled_node(
                        node,
                        builder,
                        &name,
                        registry.collected_payloads(),
                        &mut ctx,
                    )
                    .map_err(ApplyError::Apply)?;
                    let inputs = input_subs(&new, id, process.as_ref(), &self.names)
                        .map_err(ApplyError::Apply)?;
                    self.manager
                        .restart_node(&name, process, inputs)
                        .map_err(ApplyError::Apply)?;
                    summary.restarted += 1;
                }
            }
        }
        self.compiled = new;
        Ok(summary)
    }

    /// Applies the subset of an edited capture graph that can preserve an
    /// explicit future-only boundary. Phase 13.1 deliberately accepts only
    /// builder-declared hot configuration; structural changes and restarts
    /// remain in the edited graph for the next capture or ordinary Run.
    pub fn apply_configuration_epoch(
        &mut self,
        graph: &GraphState,
        registry: &BuilderRegistry,
        boundary: ConfigurationBoundary,
    ) -> Result<ApplySummary, ApplyError> {
        let mut new = lower(graph, registry).map_err(ApplyError::Compile)?;
        reuse_sampling_activities(&self.compiled, &mut new);
        cache_platform::configure_directory(&mut new, self.persistent_cache_directory.as_deref());
        let edits = diff(&self.compiled, &new, registry).map_err(ApplyError::NeedsFullRestart)?;
        if edits.is_empty() {
            self.compiled = new;
            return Ok(ApplySummary::default());
        }
        if self.cache_pruned {
            return Err(ApplyError::NeedsFullRestart(
                "the running graph reused persistent derived data; the edit is deferred to the next capture"
                    .to_string(),
            ));
        }
        if let Some(edit) = edits
            .iter()
            .find(|edit| !matches!(edit, LiveEdit::Configure(_, _)))
        {
            let reason = match edit {
                LiveEdit::Add(_) => "node additions",
                LiveEdit::Remove(_) => "node removals",
                LiveEdit::Restart(_) => "node restarts or wiring changes",
                LiveEdit::Configure(_, _) => unreachable!(),
            };
            return Err(ApplyError::NeedsFullRestart(format!(
                "{reason} are deferred to the next capture"
            )));
        }

        // Resolve every target before sending any control message so a
        // missing running node cannot leave a partially scheduled epoch.
        let scheduled: Vec<_> = edits
            .into_iter()
            .map(|edit| match edit {
                LiveEdit::Configure(id, config) => self
                    .names
                    .get(&id)
                    .cloned()
                    .map(|name| (name, config))
                    .ok_or_else(|| ApplyError::Apply(format!("n{} not running", id.0))),
                _ => unreachable!(),
            })
            .collect::<Result<_, _>>()?;
        let configured = scheduled.len();
        for (name, config) in scheduled {
            self.manager
                .reconfigure_at(&name, config, boundary)
                .map_err(ApplyError::Apply)?;
        }
        self.compiled = new;
        Ok(ApplySummary {
            configured,
            ..ApplySummary::default()
        })
    }

    pub fn is_finished(&self) -> bool {
        self.manager.is_finished()
    }

    /// Signals the wind-down and returns immediately — never joins node
    /// threads, so it is safe to call from the frame loop (a node may be
    /// mid-`work()` for a while yet; see `PipelineManager::request_stop`).
    /// [`Self::is_finished`] flips once every thread has exited.
    pub fn stop(&mut self) {
        self.stop_requested = true;
        self.manager.request_stop();
    }

    /// True from [`Self::stop`] until the run is dropped — used by the
    /// toolbar to show "Stopping…" while threads finish their current
    /// `work()` call.
    pub fn is_stopping(&self) -> bool {
        self.stop_requested
    }

    /// Drives up to `budget` `work()` calls forward. A no-op on the
    /// threaded native manager (its nodes run themselves); on wasm's
    /// cooperative manager this is what actually advances the run, so the
    /// UI frame loop must call it every frame regardless of target.
    pub fn pump(&mut self, budget: usize) {
        self.manager.pump(budget);
    }

    /// Blocks until the run completes naturally (tests / headless).
    pub fn wait(&mut self) {
        self.manager.wait();
    }

    /// Items produced per UI node (sum of `work()` returns), for header
    /// progress display.
    pub fn progress(&self) -> Vec<(NodeId, u64)> {
        let by_name: HashMap<String, u64> = self.manager.progress().into_iter().collect();
        self.names
            .iter()
            .filter_map(|(id, name)| by_name.get(name).map(|items| (*id, *items)))
            .collect()
    }

    /// Consumers dropped by backpressure policy since the last call, mapped
    /// back to UI nodes where possible.
    pub fn take_disconnected(&self) -> Vec<(Option<NodeId>, DisconnectEvent)> {
        self.manager
            .take_disconnected()
            .into_iter()
            .map(|event| {
                let id = event.consumer.as_ref().and_then(|consumer| {
                    self.names
                        .iter()
                        .find(|(_, name)| *name == consumer)
                        .map(|(id, _)| *id)
                });
                (id, event)
            })
            .collect()
    }
}

pub fn start_app_run(
    graph: &GraphState,
    registry: &BuilderRegistry,
    ctx: &mut CompileCtx,
) -> Result<LiveRun, Vec<CompileError>> {
    start_live(graph, registry, ctx)
}

/// Starts an ordinary application run while replacing explicitly identified
/// source nodes. Finalized-session replay uses this entry point so lowering
/// cannot invoke the captured provider's discovery or build paths.
pub fn start_app_run_with_source_overrides(
    graph: &GraphState,
    registry: &BuilderRegistry,
    ctx: &mut CompileCtx,
    overrides: SourceProcessOverrides,
) -> Result<LiveRun, Vec<CompileError>> {
    start_live_inner(graph, registry, ctx, overrides)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use logic_analyzer_processing::nodes::sinks::binary_file_writer::BinaryFileWriter;
    use logic_analyzer_processing::test_support::{BufferedFakeConfig, BufferedFakeProvider};
    use node_graph::{NodeDef, NodeGraphWidget};
    use signal_processing::{
        AcquisitionContext, AcquisitionResult, CaptureAnalysisChannel, CaptureAnalysisSource,
        CaptureChannelId, CaptureChunk, CaptureChunkWriter, CaptureDataDelivery,
        CaptureProviderCapabilities, CaptureSessionId, CaptureStoreCursor, ConfigValue,
        CooperativeManager, DerivedLaneData, NativeCaptureStore, NativeCaptureStoreConfig,
        NodeSpec, Pipeline, PreparedAcquisition, Sample, Trigger, TriggerCount, TriggerCountMode,
        TriggerEditorSchema, TriggerIdentifier, TriggerLogicOperator, TriggerPlacement,
        TriggerPredicate, TriggerStage, Word,
    };

    use super::*;

    fn discover_compiled_live_capture_feature(
        graph: &GraphState,
        compiled: &CompiledGraph,
        builders: &BuilderRegistry,
    ) -> Result<Option<DiscoveredLiveCaptureFeature>, LiveCaptureDiscoveryError> {
        let retained: HashSet<_> = compiled.nodes.iter().map(|node| node.id).collect();
        discover_live_capture_feature_from(graph, builders, |node| retained.contains(&node.id))
    }
    use crate::nodes;

    fn startup_widget() -> NodeGraphWidget {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        nodes::test_graphs_tests::populate_startup(&mut widget);
        widget
    }

    fn uart_demo_widget() -> NodeGraphWidget {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        nodes::test_graphs_tests::populate_uart_demo(&mut widget);
        widget
    }

    fn binary_decoder_demo_widget() -> NodeGraphWidget {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        nodes::test_graphs_tests::build_binary_decoder_demo(&mut widget);
        widget
    }

    struct ThrottledProcess {
        inner: Box<dyn ProcessNode>,
        delay: Duration,
    }

    struct ThrottledBinaryBuilder {
        delay: Duration,
    }

    struct InstrumentedCaptureBuilder {
        discovery_calls: Arc<AtomicUsize>,
        provider_build_calls: Arc<AtomicUsize>,
    }

    struct BufferedPluginBuilder;

    struct TriggerOnlyPluginBuilder;

    struct BufferedPluginGraphSourceFactory {
        channels: Arc<[CaptureChannelId]>,
    }

    struct BufferedPluginFeature {
        channels: Arc<[CaptureChannelId]>,
        channel_names: Arc<[String]>,
        capabilities: CaptureProviderCapabilities,
        provider: BufferedFakeProvider,
    }

    impl CaptureGraphSourceFactory for BufferedPluginGraphSourceFactory {
        fn create(
            &self,
            cursor: Box<dyn CaptureStoreCursor>,
        ) -> Result<Box<dyn ProcessNode>, String> {
            let channels = self
                .channels
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, channel)| {
                    CaptureAnalysisChannel::separate(
                        channel,
                        format!("ch{index}"),
                        format!("block{index}"),
                    )
                })
                .collect();
            CaptureAnalysisSource::new("buffered-plugin-analysis", cursor, 2_000_000.0, channels)
                .map(|source| Box::new(source) as Box<dyn ProcessNode>)
        }
    }

    impl LiveCaptureFeature for BufferedPluginFeature {
        fn channels(&self) -> &[CaptureChannelId] {
            &self.channels
        }

        fn channel_names(&self) -> &[String] {
            &self.channel_names
        }

        fn sample_rate_hz(&self) -> f64 {
            2_000_000.0
        }

        fn capabilities(&self) -> &CaptureProviderCapabilities {
            &self.capabilities
        }

        fn graph_source_factory(&self) -> Arc<dyn CaptureGraphSourceFactory> {
            Arc::new(BufferedPluginGraphSourceFactory {
                channels: Arc::clone(&self.channels),
            })
        }

        fn prepare(
            self: Box<Self>,
            context: AcquisitionContext,
        ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
            self.provider.prepare(context)
        }
    }

    impl RuntimeBuilder for BufferedPluginBuilder {
        fn is_source(&self) -> bool {
            true
        }

        fn accepted_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
            crate::nodes::sources::TestCaptureSourceBuilder.accepted_kinds(socket, state)
        }

        fn offered_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
            crate::nodes::sources::TestCaptureSourceBuilder.offered_kinds(socket, state)
        }

        fn input_port(
            &self,
            socket: &Socket,
            member_index: usize,
            state: &Value,
            kind: PortKind,
        ) -> Option<String> {
            crate::nodes::sources::TestCaptureSourceBuilder.input_port(
                socket,
                member_index,
                state,
                kind,
            )
        }

        fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String> {
            crate::nodes::sources::TestCaptureSourceBuilder.output_port(socket, state, kind)
        }

        fn viewer_channel_origin(&self, socket: &Socket, state: &Value) -> Option<usize> {
            crate::nodes::sources::TestCaptureSourceBuilder.viewer_channel_origin(socket, state)
        }

        fn live_capture_feature(
            &self,
            _state: &Value,
        ) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
            let channels: Arc<[CaptureChannelId]> = vec![
                CaptureChannelId::new("pod-a:3"),
                CaptureChannelId::new("pod-q:41"),
                CaptureChannelId::new("aux-bank:9"),
            ]
            .into();
            let config = BufferedFakeConfig::new(Arc::clone(&channels), 2_000_000, 19, 5, 0x8d31)
                .map_err(|error| error.to_string())?;
            let capabilities = config.capabilities().clone();
            Ok(Some(Box::new(BufferedPluginFeature {
                channel_names: vec!["Pod A 3".into(), "Pod Q 41".into(), "Aux 9".into()].into(),
                channels,
                capabilities,
                provider: BufferedFakeProvider::new(config),
            })))
        }

        fn apply_live_capture_edit(
            &self,
            state: &Value,
            edit: &LiveCaptureEdit,
        ) -> Result<Option<Value>, String> {
            match edit {
                LiveCaptureEdit::SetTriggerProgram { program } => Ok(Some(serde_json::json!({
                    "previous_state": state,
                    "received_program": program,
                }))),
                LiveCaptureEdit::SetSimpleTrigger { .. } => Ok(None),
            }
        }

        fn input_required(&self, socket: &Socket, state: &Value) -> bool {
            crate::nodes::sources::TestCaptureSourceBuilder.input_required(socket, state)
        }

        fn build(
            &self,
            name: &str,
            state: &Value,
            resolved: &ResolvedInputs,
            ctx: &mut CompileCtx,
        ) -> Result<Box<dyn ProcessNode>, String> {
            crate::nodes::sources::TestCaptureSourceBuilder.build(name, state, resolved, ctx)
        }
    }

    impl RuntimeBuilder for TriggerOnlyPluginBuilder {
        fn is_source(&self) -> bool {
            true
        }

        fn accepted_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
            crate::nodes::sources::TestCaptureSourceBuilder.accepted_kinds(socket, state)
        }

        fn offered_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
            crate::nodes::sources::TestCaptureSourceBuilder.offered_kinds(socket, state)
        }

        fn input_port(
            &self,
            socket: &Socket,
            member_index: usize,
            state: &Value,
            kind: PortKind,
        ) -> Option<String> {
            crate::nodes::sources::TestCaptureSourceBuilder.input_port(
                socket,
                member_index,
                state,
                kind,
            )
        }

        fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String> {
            crate::nodes::sources::TestCaptureSourceBuilder.output_port(socket, state, kind)
        }

        fn viewer_channel_origin(&self, socket: &Socket, state: &Value) -> Option<usize> {
            crate::nodes::sources::TestCaptureSourceBuilder.viewer_channel_origin(socket, state)
        }

        fn live_capture_feature(
            &self,
            _state: &Value,
        ) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
            panic!("trigger configuration discovery consulted the acquisition feature")
        }

        fn trigger_configuration(
            &self,
            _state: &Value,
        ) -> Result<Option<TriggerConfigurationFeature>, String> {
            let schema = TriggerEditorSchema::new(
                TriggerIdentifier::new("plugin.trigger-only").unwrap(),
                1,
                1,
                2,
                vec![TriggerLogicOperator::And],
            )
            .unwrap()
            .with_digital_conditions(vec![SimpleTriggerCondition::High])
            .unwrap();
            TriggerConfigurationFeature::new(
                schema,
                None,
                vec![SimpleTriggerChannel {
                    channel_id: CaptureChannelId::new("plugin-bank:23"),
                    viewer_channel: 0,
                    name: "Plugin 23".into(),
                    enabled: true,
                    condition: SimpleTriggerCondition::Ignore,
                }],
            )
            .map(Some)
        }

        fn input_required(&self, socket: &Socket, state: &Value) -> bool {
            crate::nodes::sources::TestCaptureSourceBuilder.input_required(socket, state)
        }

        fn build(
            &self,
            name: &str,
            state: &Value,
            resolved: &ResolvedInputs,
            ctx: &mut CompileCtx,
        ) -> Result<Box<dyn ProcessNode>, String> {
            crate::nodes::sources::TestCaptureSourceBuilder.build(name, state, resolved, ctx)
        }
    }

    impl RuntimeBuilder for InstrumentedCaptureBuilder {
        fn is_source(&self) -> bool {
            true
        }

        fn accepted_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
            crate::nodes::sources::TestCaptureSourceBuilder.accepted_kinds(socket, state)
        }

        fn offered_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
            crate::nodes::sources::TestCaptureSourceBuilder.offered_kinds(socket, state)
        }

        fn input_port(
            &self,
            socket: &Socket,
            member_index: usize,
            state: &Value,
            kind: PortKind,
        ) -> Option<String> {
            crate::nodes::sources::TestCaptureSourceBuilder.input_port(
                socket,
                member_index,
                state,
                kind,
            )
        }

        fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String> {
            crate::nodes::sources::TestCaptureSourceBuilder.output_port(socket, state, kind)
        }

        fn viewer_channel_origin(&self, socket: &Socket, state: &Value) -> Option<usize> {
            crate::nodes::sources::TestCaptureSourceBuilder.viewer_channel_origin(socket, state)
        }

        fn live_capture_feature(
            &self,
            _state: &Value,
        ) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
            self.discovery_calls.fetch_add(1, Ordering::SeqCst);
            Err("replay attempted provider discovery".into())
        }

        fn input_required(&self, socket: &Socket, state: &Value) -> bool {
            crate::nodes::sources::TestCaptureSourceBuilder.input_required(socket, state)
        }

        fn build(
            &self,
            _name: &str,
            _state: &Value,
            _resolved: &ResolvedInputs,
            _ctx: &mut CompileCtx,
        ) -> Result<Box<dyn ProcessNode>, String> {
            self.provider_build_calls.fetch_add(1, Ordering::SeqCst);
            Err("replay attempted to build the provider source".into())
        }
    }

    impl RuntimeBuilder for ThrottledBinaryBuilder {
        fn accepted_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
            crate::nodes::decoders::BinaryDecoderBuilder.accepted_kinds(socket, state)
        }

        fn offered_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
            crate::nodes::decoders::BinaryDecoderBuilder.offered_kinds(socket, state)
        }

        fn input_port(
            &self,
            socket: &Socket,
            member_index: usize,
            state: &Value,
            kind: PortKind,
        ) -> Option<String> {
            crate::nodes::decoders::BinaryDecoderBuilder.input_port(
                socket,
                member_index,
                state,
                kind,
            )
        }

        fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String> {
            crate::nodes::decoders::BinaryDecoderBuilder.output_port(socket, state, kind)
        }

        fn word_display_format(&self, socket: &Socket, state: &Value) -> Option<String> {
            crate::nodes::decoders::BinaryDecoderBuilder.word_display_format(socket, state)
        }

        fn sampling_overlay(&self, state: &Value) -> Option<SamplingOverlayDescriptor> {
            crate::nodes::decoders::BinaryDecoderBuilder.sampling_overlay(state)
        }

        fn input_required(&self, socket: &Socket, state: &Value) -> bool {
            crate::nodes::decoders::BinaryDecoderBuilder.input_required(socket, state)
        }

        fn build(
            &self,
            name: &str,
            state: &Value,
            resolved: &ResolvedInputs,
            ctx: &mut CompileCtx,
        ) -> Result<Box<dyn ProcessNode>, String> {
            let inner =
                crate::nodes::decoders::BinaryDecoderBuilder.build(name, state, resolved, ctx)?;
            Ok(Box::new(ThrottledProcess {
                inner,
                delay: self.delay,
            }))
        }
    }

    impl ProcessNode for ThrottledProcess {
        fn name(&self) -> &str {
            self.inner.name()
        }

        fn should_stop(&self) -> bool {
            self.inner.should_stop()
        }

        fn num_inputs(&self) -> usize {
            self.inner.num_inputs()
        }

        fn num_outputs(&self) -> usize {
            self.inner.num_outputs()
        }

        fn input_schema(&self) -> Vec<signal_processing::PortSchema> {
            self.inner.input_schema()
        }

        fn output_schema(&self) -> Vec<signal_processing::PortSchema> {
            self.inner.output_schema()
        }

        fn work(
            &mut self,
            inputs: &[signal_processing::InputPort],
            outputs: &[signal_processing::OutputPort],
        ) -> signal_processing::WorkResult<usize> {
            std::thread::sleep(self.delay);
            self.inner.work(inputs, outputs)
        }
    }

    fn live_analysis_chunk(
        session_id: CaptureSessionId,
        channels: &[CaptureChannelId],
        sequence: u64,
        start_sample: u64,
        sample_count: u64,
    ) -> CaptureChunk {
        let bit_offset = (sequence % 7) as u8;
        let bit_count = sample_count as usize * channels.len();
        let mut bytes = vec![0_u8; (usize::from(bit_offset) + bit_count).div_ceil(8)];
        for relative in 0..sample_count {
            let sample = start_sample + relative;
            for channel in 0..channels.len() {
                let value = match channel {
                    0 => !sample.is_multiple_of(2),
                    1 => !(sample / 2).is_multiple_of(2),
                    _ => false,
                };
                if value {
                    let bit =
                        usize::from(bit_offset) + relative as usize * channels.len() + channel;
                    bytes[bit / 8] |= 1 << (bit % 8);
                }
            }
        }
        CaptureChunk::packed_lsb_first(
            session_id,
            sequence,
            start_sample,
            sample_count,
            channels.to_vec(),
            bytes,
            bit_offset,
        )
        .unwrap()
    }

    fn captured_words(
        lanes: &signal_processing::DerivedLanes,
    ) -> Vec<signal_processing::Annotation> {
        let lanes = lanes.read();
        lanes
            .iter()
            .find_map(|lane| match &lane.data {
                DerivedLaneData::Annotations(words) => Some(words.clone()),
                DerivedLaneData::IndexedAnnotations(indexed) => {
                    let metadata = indexed.metadata();
                    let end = metadata.extent_end_ns.unwrap_or(0);
                    Some(
                        indexed
                            .query()
                            .exact_window(
                                0,
                                end,
                                usize::try_from(metadata.total_word_count)
                                    .unwrap_or(usize::MAX)
                                    .saturating_add(1),
                            )
                            .unwrap()
                            .annotations,
                    )
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("binary decoder word lane; actual lanes: {lanes:?}"))
    }

    fn annotation_bytes(annotations: &[signal_processing::Annotation]) -> Vec<u8> {
        annotations
            .iter()
            .flat_map(|annotation| {
                [
                    annotation.start_ns.to_le_bytes(),
                    annotation.end_ns.to_le_bytes(),
                    annotation.value.to_le_bytes(),
                ]
                .into_iter()
                .flatten()
            })
            .collect()
    }

    #[test]
    fn lagging_live_analysis_and_finalized_replay_are_byte_equal_without_provider_operations() {
        const CHUNKS: u64 = 48;
        const SAMPLES_PER_CHUNK: u64 = 128;

        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source_node = nodes::test_graphs_tests::build_live_binary_test(&mut widget);
        let captured_feature =
            discover_live_capture_feature(widget.graph(), &BuilderRegistry::standard())
                .unwrap()
                .expect("test graph has a live capture feature");
        assert_eq!(captured_feature.source_node, source_node);
        let graph_source_factory = captured_feature.graph_source_factory();
        let mut registry = BuilderRegistry::standard();
        registry.insert(
            nodes::BinaryDecoder::name(),
            Box::new(ThrottledBinaryBuilder {
                delay: Duration::from_millis(3),
            }),
        );
        let channels = captured_feature.channels().to_vec();
        let session_id = CaptureSessionId::new(0x4c49_5645);
        let directory = tempfile::tempdir().unwrap();
        let descriptor =
            signal_processing::CaptureStoreDescriptor::new(session_id, channels.clone()).unwrap();
        let (store, mut writer) =
            NativeCaptureStore::create(NativeCaptureStoreConfig::new(directory.path(), descriptor))
                .unwrap();

        let cursor = store.open_cursor().unwrap();
        let source = graph_source_factory.create(Box::new(cursor)).unwrap();
        let mut live_ctx = CompileCtx::default();
        let live_lanes = live_ctx.derived_lanes.clone();
        let mut live_run = start_live_analysis(
            widget.graph(),
            &registry,
            &mut live_ctx,
            LiveAnalysisSource {
                source_node,
                process: source,
            },
        )
        .unwrap();

        for sequence in 0..CHUNKS {
            writer
                .append(live_analysis_chunk(
                    session_id,
                    &channels,
                    sequence,
                    sequence * SAMPLES_PER_CHUNK,
                    SAMPLES_PER_CHUNK,
                ))
                .unwrap();
        }
        writer.finish().unwrap();
        drop(writer);
        let committed_samples = CHUNKS * SAMPLES_PER_CHUNK;
        assert_eq!(store.snapshot().committed_samples, committed_samples);
        let processed_while_capture_finished = live_run
            .progress()
            .into_iter()
            .find_map(|(node, items)| (node == source_node).then_some(items))
            .unwrap_or(0);
        assert!(
            processed_while_capture_finished < committed_samples,
            "throttled analysis unexpectedly kept up with acquisition"
        );
        let finalized = store.finalize().unwrap();
        while !live_run.is_finished() {
            std::thread::yield_now();
        }
        let final_processed = live_run
            .progress()
            .into_iter()
            .find_map(|(node, items)| (node == source_node).then_some(items));
        live_run.wait();
        let live_words = captured_words(&live_lanes);
        assert!(!live_words.is_empty());

        let replay_source = graph_source_factory
            .create(Box::new(finalized.open_cursor().unwrap()))
            .unwrap();
        let mut reference_ctx = CompileCtx::default();
        let reference_lanes = reference_ctx.derived_lanes.clone();
        let discovery_calls = Arc::new(AtomicUsize::new(0));
        let provider_build_calls = Arc::new(AtomicUsize::new(0));
        let mut reference_registry = BuilderRegistry::standard();
        reference_registry.insert(
            nodes::TestCaptureSource::name(),
            Box::new(InstrumentedCaptureBuilder {
                discovery_calls: Arc::clone(&discovery_calls),
                provider_build_calls: Arc::clone(&provider_build_calls),
            }),
        );
        let mut overrides = SourceProcessOverrides::new();
        overrides.insert(source_node, replay_source);
        let mut reference_run = start_app_run_with_source_overrides(
            widget.graph(),
            &reference_registry,
            &mut reference_ctx,
            overrides,
        )
        .unwrap();
        reference_run.wait();

        let replay_words = captured_words(&reference_lanes);
        assert_eq!(
            annotation_bytes(&live_words),
            annotation_bytes(&replay_words)
        );
        assert_eq!(final_processed, Some(committed_samples));
        assert_eq!(discovery_calls.load(Ordering::SeqCst), 0);
        assert_eq!(provider_build_calls.load(Ordering::SeqCst), 0);
    }

    fn run_cooperatively(widget: &NodeGraphWidget) -> (CompiledGraph, Vec<(String, u64)>) {
        let registry = BuilderRegistry::standard();
        let cache_directory = tempfile::tempdir().unwrap();
        let mut compiled = lower(widget.graph(), &registry).unwrap();
        cache_platform::configure_directory(&mut compiled, Some(cache_directory.path()));
        let mut manager = CooperativeManager::new();
        let mut names = HashMap::new();
        let mut ctx = CompileCtx {
            sampling_activities: sampling_activity_map(&compiled),
            ..CompileCtx::default()
        };

        for id in topo_order(&compiled) {
            let node = compiled_node(&compiled, id);
            let builder = registry.get(&node.builder).unwrap();
            ctx.derived_word_caches
                .clone_from(&node.derived_word_caches);
            register_collected_subscribers(node, builder, &node.runtime_name, &ctx).unwrap();
            let process = materialize_compiled_node(
                node,
                builder,
                &node.runtime_name,
                registry.collected_payloads(),
                &mut ctx,
            )
            .unwrap();
            let inputs = input_subs(&compiled, id, process.as_ref(), &names).unwrap();
            manager
                .add_node_deferred(NodeSpec {
                    name: node.runtime_name.clone(),
                    node: process,
                    inputs,
                })
                .unwrap();
            names.insert(id, node.runtime_name.clone());
        }

        manager.start_all_deferred().unwrap();
        for _ in 0..1_000 {
            manager.pump(256);
            if manager.is_finished() {
                break;
            }
        }
        assert!(
            manager.is_finished(),
            "unfinished: {:?}",
            manager.progress()
        );
        (compiled, manager.progress())
    }

    #[test]
    fn startup_graph_lowers() {
        let widget = startup_widget();
        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));

        // Every saved processing node has a runtime, plus the generic viewer
        // sink synthesized from watched outputs.
        assert_eq!(compiled.nodes.len(), 11);
        assert_eq!(compiled.edges.len(), 30);

        let spi_sampling = compiled
            .sampling_overlays
            .iter()
            .find(|candidate| candidate.node_title == "SPI Decoder")
            .expect("SPI decoder should expose a sampling overlay");
        assert_eq!(spi_sampling.overlay.edge, SamplingEdge::Rising);
        assert!(!spi_sampling.overlay.sampled_channels.is_empty());
        assert!(
            !spi_sampling.overlay.qualifiers.is_empty()
                || !spi_sampling.overlay.activities.is_empty()
        );
        let binary_sampling = compiled
            .sampling_overlays
            .iter()
            .find(|candidate| candidate.node_title == "Binary Decoder")
            .expect("binary decoder should expose a sampling overlay");
        assert_eq!(binary_sampling.overlay.edge, SamplingEdge::Both);
        assert!(!binary_sampling.overlay.sampled_channels.is_empty());
        assert!(
            !binary_sampling.overlay.qualifiers.is_empty()
                || !binary_sampling.overlay.activities.is_empty()
        );

        // Viewer lanes resolve with per-lane kinds and producer labels.
        let viewer = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "Viewer")
            .unwrap();
        let lanes = viewer.resolved.members(0);
        assert_eq!(lanes.len(), 7);
        assert!(
            lanes
                .iter()
                .any(|(_, input)| input.kind == PortKind::of::<Word>()
                    && input.source == "SPI Decoder.MOSI Bits")
        );
        assert!(
            lanes
                .iter()
                .any(|(_, input)| input.kind == PortKind::of::<Word>()
                    && input.source == "SPI Decoder.MOSI Data")
        );
        assert!(
            lanes
                .iter()
                .any(|(_, input)| input.kind == PortKind::of::<Trigger>()
                    && input.source == "Match Start.Match")
        );
        assert!(
            lanes
                .iter()
                .any(|(_, input)| input.kind == PortKind::of::<Word>()
                    && input.source == "Binary Decoder.Words")
        );

        // Kind negotiation spot checks: SPI clk reads edges, the binary
        // decoder reads blocks — both fed from the same UI sockets.
        let spi = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "SPI Decoder")
            .unwrap();
        let decoder = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "Binary Decoder")
            .unwrap();
        let edge_to = |node: NodeId, port: &str| {
            compiled
                .edges
                .iter()
                .find(|e| e.to.0 == node && e.to.1 == port)
                .unwrap_or_else(|| panic!("no edge into {port}"))
        };
        // The runtime port name no longer encodes which kind was picked
        // (both resolve to `ch{channel}` on a single collapsed port —
        // see `FileSourceBuilder::output_port`), so check the negotiated
        // kind directly via each node's `ResolvedInputs` instead of
        // sniffing a `d`/`b` prefix.
        assert_eq!(spi.resolved.kind(0), Some(PortKind::of::<Sample>())); // clk
        assert_eq!(
            decoder.resolved.kind(0),
            Some(PortKind::of::<SampleBlock>())
        ); // strobe
        assert_eq!(edge_to(decoder.id, "strobe").buffer, 2);
        assert_eq!(edge_to(spi.id, "clk").buffer, 10_000_000);
        assert_eq!(edge_to(decoder.id, "d7").from.1, "ch7");
        assert!(
            compiled
                .edges
                .iter()
                .any(|e| e.to.1 == "enable_signal" && e.buffer == 1_000)
        );
    }

    #[test]
    fn development_capture_feature_is_discovered_without_node_name_matching() {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at(nodes::TestLiveCaptureSource::name(), Pos2::ZERO)
            .unwrap();
        let feature = discover_live_capture_feature(widget.graph(), &BuilderRegistry::standard())
            .unwrap()
            .unwrap();

        assert_eq!(feature.source_node, source);
        assert_eq!(feature.channels().len(), 11);
        assert_eq!(feature.channels()[7].as_str(), "demo:7");
        assert_eq!(feature.simple_trigger_channels().len(), 11);
        assert!(
            feature
                .simple_trigger_channels()
                .iter()
                .all(|channel| channel.condition == SimpleTriggerCondition::Ignore)
        );

        let state = apply_live_capture_edit(
            widget.graph(),
            &BuilderRegistry::standard(),
            source,
            &LiveCaptureEdit::SetSimpleTrigger {
                channel_id: CaptureChannelId::new("demo:7"),
                condition: SimpleTriggerCondition::Falling,
            },
        )
        .unwrap();
        assert!(widget.edit_node_state(source, state));
        let edited = discover_live_capture_feature(widget.graph(), &BuilderRegistry::standard())
            .unwrap()
            .unwrap();
        assert_eq!(
            edited.simple_trigger_channels()[7].condition,
            SimpleTriggerCondition::Falling
        );
        assert!(edited.has_simple_trigger());
        let trigger_configuration =
            discover_trigger_configuration(widget.graph(), &BuilderRegistry::standard())
                .unwrap()
                .unwrap();
        assert_eq!(trigger_configuration.source_node, source);
        assert_eq!(
            trigger_configuration.feature.program(),
            edited.trigger_program()
        );
        let panel_program = trigger_configuration
            .feature
            .schema()
            .simple_program([
                (
                    CaptureChannelId::new("demo:2"),
                    SimpleTriggerCondition::High,
                ),
                (
                    CaptureChannelId::new("demo:7"),
                    SimpleTriggerCondition::Falling,
                ),
            ])
            .unwrap();
        let state = apply_live_capture_edit(
            widget.graph(),
            &BuilderRegistry::standard(),
            source,
            &LiveCaptureEdit::SetTriggerProgram {
                program: panel_program.clone(),
            },
        )
        .unwrap();
        assert!(widget.edit_node_state(source, state));
        let panel_edited =
            discover_trigger_configuration(widget.graph(), &BuilderRegistry::standard())
                .unwrap()
                .unwrap();
        assert_eq!(panel_edited.feature.program(), panel_program.as_ref());

        let serialized = serde_json::to_string(widget.graph()).unwrap();
        let graph: GraphState = serde_json::from_str(&serialized).unwrap();
        assert!(
            graph.nodes[&source]
                .state
                .get("trigger_conditions")
                .is_none()
        );
        let mut restored = NodeGraphWidget::new(nodes::build_registry());
        restored.set_graph(graph);
        let reloaded =
            discover_live_capture_feature(restored.graph(), &BuilderRegistry::standard())
                .unwrap()
                .unwrap();
        assert_eq!(
            reloaded.simple_trigger_channels()[7].condition,
            SimpleTriggerCondition::Falling
        );
        assert_eq!(
            reloaded.simple_trigger_channels()[2].condition,
            SimpleTriggerCondition::High
        );
    }

    #[test]
    fn trigger_configuration_discovery_does_not_require_acquisition() {
        let mut node_types = nodes::build_registry();
        let mut builders = BuilderRegistry::standard();
        crate::PluginContext::new(&mut node_types, &mut builders).register_builder(
            nodes::TestCaptureSource::name(),
            Box::new(TriggerOnlyPluginBuilder),
        );
        let mut widget = NodeGraphWidget::new(node_types);
        let source = widget
            .add_node_at(nodes::TestCaptureSource::name(), Pos2::ZERO)
            .unwrap();

        let configuration = discover_trigger_configuration(widget.graph(), &builders)
            .unwrap()
            .unwrap();

        assert_eq!(configuration.source_node, source);
        assert_eq!(
            configuration.feature.schema().id().as_str(),
            "plugin.trigger-only"
        );
        assert_eq!(
            configuration.feature.channels()[0].channel_id.as_str(),
            "plugin-bank:23"
        );
    }

    #[test]
    fn advanced_test_graph_edit_executes_identically_after_json_reload() {
        let builders = BuilderRegistry::standard();
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at(nodes::TestLiveCaptureSource::name(), Pos2::ZERO)
            .unwrap();
        let configuration = discover_trigger_configuration(widget.graph(), &builders)
            .unwrap()
            .unwrap();
        let schema = configuration.feature.schema();
        let program = TriggerProgram::new(
            schema.id().clone(),
            schema.revision(),
            vec![
                TriggerStage {
                    predicates: vec![TriggerPredicate::Digital {
                        channel: CaptureChannelId::new("demo:0"),
                        condition: SimpleTriggerCondition::High,
                    }],
                    logic: TriggerLogicOperator::And,
                    inverted: false,
                    count: Some(TriggerCount {
                        mode: TriggerCountMode::Occurrences,
                        value: 2,
                    }),
                },
                TriggerStage {
                    predicates: vec![TriggerPredicate::Digital {
                        channel: CaptureChannelId::new("demo:0"),
                        condition: SimpleTriggerCondition::Falling,
                    }],
                    logic: TriggerLogicOperator::Or,
                    inverted: true,
                    count: Some(TriggerCount {
                        mode: TriggerCountMode::Consecutive,
                        value: 1,
                    }),
                },
            ],
        );
        let state = apply_live_capture_edit(
            widget.graph(),
            &builders,
            source,
            &LiveCaptureEdit::SetTriggerProgram {
                program: Some(program.clone()),
            },
        )
        .unwrap();
        assert!(widget.edit_node_state(source, state));

        let before = discover_live_capture_feature(widget.graph(), &builders)
            .unwrap()
            .unwrap();
        assert_eq!(
            before
                .session_plan()
                .unwrap()
                .policy
                .effective
                .trigger_placement,
            Some(TriggerPlacement::SamplesBefore(5))
        );

        let graph: GraphState =
            serde_json::from_str(&serde_json::to_string(widget.graph()).unwrap()).unwrap();
        let mut restored = NodeGraphWidget::new(nodes::build_registry());
        restored.set_graph(graph);
        let restored_configuration = discover_trigger_configuration(restored.graph(), &builders)
            .unwrap()
            .unwrap();
        assert_eq!(restored_configuration.feature.program(), Some(&program));
        let after = discover_live_capture_feature(restored.graph(), &builders)
            .unwrap()
            .unwrap();
        assert_eq!(after.session_plan(), before.session_plan());
    }

    #[test]
    fn legacy_development_capture_state_migrates_with_a_visible_warning() {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at(nodes::TestCaptureSource::name(), Pos2::ZERO)
            .unwrap();
        let mut graph = widget.graph().clone();
        graph.nodes.get_mut(&source).unwrap().state = serde_json::json!({});

        let mut restored = NodeGraphWidget::new(nodes::build_registry());
        restored.set_graph(graph);
        let warning = restored.graph().nodes[&source]
            .badge
            .as_ref()
            .expect("legacy state must surface its migration");
        assert!(warning.text.contains("legacy"));
    }

    #[test]
    fn discovery_rejects_multiple_live_capture_features() {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let first = widget
            .add_node_at(nodes::TestLiveCaptureSource::name(), Pos2::ZERO)
            .unwrap();
        let second = widget
            .add_node_at(nodes::TestLiveCaptureSource::name(), Pos2::new(100.0, 0.0))
            .unwrap();

        let error = discover_live_capture_feature(widget.graph(), &BuilderRegistry::standard())
            .err()
            .unwrap();
        assert_eq!(error.source_nodes, [first, second]);
        assert!(error.message.contains("multiple"));
    }

    #[test]
    fn compiled_discovery_ignores_a_disconnected_live_feature() {
        let mut widget = uart_demo_widget();
        widget
            .add_node_at(
                nodes::TestLiveCaptureSource::name(),
                Pos2::new(1_000.0, 0.0),
            )
            .unwrap();
        let builders = BuilderRegistry::standard();
        let compiled = lower(widget.graph(), &builders).unwrap();

        assert!(
            discover_compiled_live_capture_feature(widget.graph(), &compiled, &builders)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unchanged_live_lowering_reuses_runtime_sampling_activity() {
        let widget = startup_widget();
        let registry = BuilderRegistry::standard();
        let old = lower(widget.graph(), &registry).unwrap();
        let activity = old
            .sampling_overlays
            .iter()
            .find(|candidate| candidate.node_title == "Binary Decoder")
            .and_then(|candidate| candidate.overlay.activities.first())
            .expect("startup binary enable is a runtime sampling condition")
            .clone();
        activity.record_interval(100, 200);

        let mut new = lower(widget.graph(), &registry).unwrap();
        reuse_sampling_activities(&old, &mut new);
        let reused = new
            .sampling_overlays
            .iter()
            .find(|candidate| candidate.node_title == "Binary Decoder")
            .and_then(|candidate| candidate.overlay.activities.first())
            .unwrap();
        assert!(reused.is_active_at(150));
    }

    /// A lone source node with no explicit sink, used to verify that source
    /// presentation remains independent of selectable waveform presentations.
    fn source_only_widget() -> NodeGraphWidget {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        widget
            .add_node_at(nodes::TestUartSource::name(), egui::Pos2::ZERO)
            .expect("Test UART Source is registered");
        widget
    }

    fn watch_first_output(widget: &mut NodeGraphWidget) -> NodeId {
        let id = *widget.graph().nodes.keys().next().unwrap();
        widget.graph_mut().nodes.get_mut(&id).unwrap().outputs[0].show_in_view = true;
        id
    }

    fn selectable_output_widget() -> NodeGraphWidget {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        nodes::test_graphs_tests::build_live_binary_test(&mut widget);
        widget
    }

    fn first_watched_selectable_output(widget: &NodeGraphWidget) -> (NodeId, usize) {
        widget
            .graph()
            .nodes
            .values()
            .find_map(|node| {
                node.outputs
                    .iter()
                    .position(|output| output.view_selectable && output.show_in_view)
                    .map(|index| (node.id, index))
            })
            .expect("test graph has a watched selectable output")
    }

    #[test]
    fn unwatched_source_has_no_sink() {
        let widget = source_only_widget();
        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(errors.iter().any(|e| e.message.contains("no sink")));
    }

    #[test]
    fn non_selectable_source_output_ignores_a_legacy_view_flag() {
        let mut widget = source_only_widget();
        watch_first_output(&mut widget);

        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(errors.iter().any(|error| error.message.contains("no sink")));
    }

    #[test]
    fn counter_and_formatter_outputs_can_be_watched() {
        use signal_processing::{NumberSample, TextSample};

        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        nodes::test_graphs_tests::build_binary_decoder_demo(&mut widget);
        for definition in [nodes::Counter::name(), nodes::StringFormatter::name()] {
            let node = widget
                .graph_mut()
                .nodes
                .values_mut()
                .find(|node| node.def_name() == definition)
                .unwrap_or_else(|| panic!("missing {definition}"));
            node.outputs[0].show_in_view = true;
        }

        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));
        let viewer = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .expect("synthetic viewer");
        let kinds: Vec<_> = viewer
            .resolved
            .members(0)
            .into_iter()
            .map(|(_, input)| input.kind)
            .collect();
        assert!(kinds.contains(&PortKind::of::<NumberSample>()));
        assert!(kinds.contains(&PortKind::of::<TextSample>()));

        let mut ctx = CompileCtx::default();
        let derived = ctx.derived_lanes.clone();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx)
            .expect("watched value levels should run");
        run.wait();
        let lanes = derived.read();
        for (suffix, expected_kind) in [
            (".Count", signal_processing::CollectedValueKind::Number),
            (".Text", signal_processing::CollectedValueKind::Text),
        ] {
            let lane = lanes
                .iter()
                .find(|lane| lane.name.ends_with(suffix))
                .unwrap_or_else(|| panic!("missing {suffix} viewer lane"));
            let signal_processing::DerivedLaneData::Values(values) = &lane.data else {
                panic!("{suffix} should be a value lane");
            };
            assert_eq!(values.kind, expected_kind);
            assert!(values.values.len() > 1, "{suffix} should contain changes");
        }
    }

    #[test]
    fn unwatching_the_only_output_drops_the_synthetic_viewer() {
        let mut widget = selectable_output_widget();
        let (node_id, output_index) = first_watched_selectable_output(&widget);
        let registry = BuilderRegistry::standard();
        assert!(lower(widget.graph(), &registry).is_ok());

        widget.graph_mut().nodes.get_mut(&node_id).unwrap().outputs[output_index].show_in_view =
            false;
        let compiled = lower(widget.graph(), &registry).unwrap();
        assert!(compiled.nodes.iter().all(|node| node.builder != "Viewer"));
        assert!(compiled.nodes.iter().any(|node| node.data_collector));

        let mut ctx = CompileCtx::default();
        let lanes = ctx.derived_lanes().clone();
        let tables = ctx.decoder_tables().clone();
        let mut run = start_live(widget.graph(), &registry, &mut ctx).unwrap();
        run.wait();

        // Presentation subscribers are reconstructible after production has
        // finished because the collector retains the data independently.
        tables.clear();
        for node in compiled.nodes.iter().filter(|node| node.data_collector) {
            let builder = registry.get(&node.builder).unwrap();
            crate::decoder_table::subscribe_collected_tables(
                node.id,
                &node.resolved,
                &builder.collected_lane_names(&node.state, &node.resolved),
                &tables,
            );
        }
        let sources = tables.read();
        assert!(!sources.is_empty());
        assert!(
            sources
                .iter()
                .flat_map(|source| &source.columns)
                .all(|column| {
                    lanes
                        .read()
                        .iter()
                        .any(|lane| lane.name == column.lane.as_str())
                })
        );
    }

    #[test]
    fn synthetic_viewer_id_is_stable_across_relowers() {
        let widget = selectable_output_widget();
        let registry = BuilderRegistry::standard();

        let first = lower(widget.graph(), &registry).unwrap();
        let second = lower(widget.graph(), &registry).unwrap();
        let viewer_id = |compiled: &CompiledGraph| {
            compiled
                .nodes
                .iter()
                .find(|n| n.builder == "Viewer")
                .unwrap()
                .id
        };
        assert_eq!(viewer_id(&first), AUTO_VIEW_NODE_ID);
        assert_eq!(viewer_id(&first), viewer_id(&second));
    }

    #[test]
    fn viewer_is_a_presentation_subscription_not_a_runtime_sink() {
        let registry = BuilderRegistry::standard();
        let builder = registry.get("Viewer").unwrap();
        assert!(!builder.is_sink());
        assert!(!builder.is_data_collector());
        assert!(builder.is_data_subscription());

        let compiled = lower(uart_demo_widget().graph(), &registry).unwrap();
        let viewer = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .unwrap();
        assert!(viewer.data_collector, "lowering must plan retained storage");

        let mut ctx = CompileCtx::default();
        let process = materialize_compiled_node(
            viewer,
            builder,
            &viewer.runtime_name,
            registry.collected_payloads(),
            &mut ctx,
        )
        .unwrap();
        assert_eq!(process.num_inputs(), viewer.resolved.member_count(0));
        assert_eq!(process.num_outputs(), 0);
    }

    fn persistent_word_keys(compiled: &CompiledGraph) -> Vec<[u8; 32]> {
        compiled
            .nodes
            .iter()
            .flat_map(|node| node.derived_word_caches.iter().flatten())
            .map(|config| config.cache_key)
            .collect()
    }

    #[test]
    fn persistent_derived_lane_key_is_stable_but_decoder_configuration_invalidates_it() {
        let mut widget = uart_demo_widget();
        let registry = BuilderRegistry::standard();
        let first = lower(widget.graph(), &registry).unwrap();
        let repeated = lower(widget.graph(), &registry).unwrap();
        let first_keys = persistent_word_keys(&first);
        assert!(!first_keys.is_empty());
        assert_eq!(first_keys, persistent_word_keys(&repeated));

        let decoder = widget
            .graph()
            .nodes
            .values()
            .find(|node| node.def_name() == "UART Decoder")
            .unwrap()
            .id;
        let mut state: nodes::UartDecoderState =
            serde_json::from_value(widget.graph().nodes[&decoder].state.clone()).unwrap();
        state.data_bits.value -= 1;
        widget.set_node_state(decoder, serde_json::to_value(state).unwrap());
        let changed = lower(widget.graph(), &registry).unwrap();
        assert_ne!(first_keys, persistent_word_keys(&changed));
    }

    #[test]
    fn cache_inventory_maps_a_lane_to_its_collector_and_upstream_nodes() {
        let widget = uart_demo_widget();
        let registry = BuilderRegistry::standard();
        let compiled = lower(widget.graph(), &registry).unwrap();
        let viewer = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .unwrap();
        let expected: Vec<_> = viewer
            .derived_word_caches
            .iter()
            .flatten()
            .map(|config| config.cache_key)
            .collect();

        let inventory =
            derived_cache_configs_by_node(widget.graph(), &registry, std::path::Path::new("cache"))
                .unwrap();
        let actual = inventory[&viewer.id]
            .iter()
            .map(|config| config.cache_key)
            .collect::<Vec<_>>();
        let decoder = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "UART Decoder")
            .unwrap();

        assert!(!expected.is_empty());
        assert!(expected.iter().all(|key| actual.contains(key)));
        let decoder_keys = inventory[&decoder.id]
            .iter()
            .map(|config| config.cache_key)
            .collect::<Vec<_>>();
        assert!(expected.iter().all(|key| decoder_keys.contains(key)));
    }

    #[test]
    fn persistent_derived_lane_key_includes_variadic_member_order() {
        let compiled = lower(uart_demo_widget().graph(), &BuilderRegistry::standard()).unwrap();
        let viewer = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .unwrap();
        let edge = compiled
            .edges
            .iter()
            .find(|edge| edge.to.0 == viewer.id && edge.kind == PortKind::of::<Word>())
            .unwrap();
        assert_ne!(
            cache_platform::persistent_lane_key(&compiled, viewer.id, 0, edge),
            cache_platform::persistent_lane_key(&compiled, viewer.id, 1, edge)
        );
    }

    #[test]
    fn persistent_cache_hit_prunes_decoder_used_only_by_cached_derived_lane() {
        use signal_processing::{IndexedAnnotationWriter, LiveStoreConfig};

        let directory = tempfile::tempdir().unwrap();
        let registry = BuilderRegistry::standard();
        let mut compiled = lower(uart_demo_widget().graph(), &registry).unwrap();
        cache_platform::configure_directory(&mut compiled, Some(directory.path()));
        let caches = compiled
            .nodes
            .iter()
            .filter(|node| node.data_collector)
            .flat_map(|node| node.derived_word_caches.iter().flatten().cloned())
            .collect::<Vec<_>>();
        assert!(!caches.is_empty());
        for cache in caches {
            let (mut writer, store) = IndexedAnnotationWriter::create(LiveStoreConfig {
                directory: directory.path().to_path_buf(),
                persistence: Some(cache),
                ..LiveStoreConfig::default()
            })
            .unwrap();
            writer.append(Word::new(0x48, 0)).unwrap();
            writer.finish().unwrap();
            drop((writer, store));
        }

        let (execution, pruned) = cache_platform::prepare_execution(&compiled, &registry);

        assert!(pruned);
        assert!(
            execution
                .nodes
                .iter()
                .all(|node| node.builder != "UART Decoder")
        );
        assert!(execution.nodes.iter().any(|node| node.builder == "Viewer"));
        assert!(
            execution
                .edges
                .iter()
                .all(|edge| edge.kind != PortKind::of::<Word>())
        );
    }

    #[test]
    fn second_live_run_reuses_persistent_words_without_starting_decoder() {
        let directory = tempfile::tempdir().unwrap();
        let widget = uart_demo_widget();
        let registry = BuilderRegistry::standard();
        let decoder_id = widget
            .graph()
            .nodes
            .values()
            .find(|node| node.def_name() == "UART Decoder")
            .unwrap()
            .id;
        let mut first_ctx = CompileCtx {
            persistent_cache_directory: Some(directory.path().to_path_buf()),
            ..CompileCtx::default()
        };
        let mut first = start_live(widget.graph(), &registry, &mut first_ctx).unwrap();
        first.wait();
        assert!(first.names.contains_key(&decoder_id));
        drop((first, first_ctx));

        let mut second_ctx = CompileCtx {
            persistent_cache_directory: Some(directory.path().to_path_buf()),
            ..CompileCtx::default()
        };
        let lanes = second_ctx.derived_lanes.clone();
        let mut second = start_live(widget.graph(), &registry, &mut second_ctx).unwrap();
        assert!(!second.names.contains_key(&decoder_id));
        second.wait();

        let lanes = lanes.read();
        assert!(lanes.iter().any(|lane| {
            matches!(lane.data, signal_processing::DerivedLaneData::Annotations(ref words) if words.len() >= 6)
                || matches!(lane.data, signal_processing::DerivedLaneData::IndexedAnnotations(_))
        }));
    }

    #[test]
    fn test_uart_graph_lowers() {
        let widget = uart_demo_widget();
        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));

        assert_eq!(compiled.nodes.len(), 4);
        assert_eq!(compiled.edges.len(), 4);
        assert_eq!(
            compiled.derived_data_retention,
            DerivedDataRetention::Unlimited
        );
        assert!(
            compiled
                .nodes
                .iter()
                .any(|n| n.builder == "Test UART Source")
        );
        assert!(compiled.nodes.iter().any(|n| n.builder == "UART Decoder"));

        let viewer = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "Viewer")
            .unwrap();
        let lanes = viewer.resolved.members(0);
        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0].1.kind, PortKind::of::<Sample>());
        assert_eq!(lanes[1].1.kind, PortKind::of::<Word>());
    }

    #[test]
    fn uart_bits_view_completes_under_cooperative_runner() {
        let mut widget = uart_demo_widget();
        let decoder = node_by_def(&widget, "UART Decoder");
        let bits = output_index(&widget, decoder, "Bits");
        widget.graph_mut().nodes.get_mut(&decoder).unwrap().outputs[bits].show_in_view = true;

        let (compiled, _) = run_cooperatively(&widget);
        assert!(
            compiled
                .edges
                .iter()
                .any(|edge| edge.from == (decoder, "bits".to_owned()))
        );
    }

    #[test]
    fn binary_decoder_demo_decodes_both_protocols_cooperatively() {
        let widget = binary_decoder_demo_widget();
        let (compiled, progress) = run_cooperatively(&widget);
        let items_for = |builder_name: &str| {
            let runtime_name = &compiled
                .nodes
                .iter()
                .find(|node| node.builder == builder_name)
                .unwrap()
                .runtime_name;
            progress
                .iter()
                .find(|(name, _)| name == runtime_name)
                .unwrap()
                .1
        };

        assert_eq!(items_for("Sigrok File Source"), 60_000);
        assert_eq!(items_for("SPI Decoder"), 60);
        assert_eq!(items_for("Binary Decoder"), 96);

        let binary_sampling = compiled
            .sampling_overlays
            .iter()
            .find(|candidate| candidate.node_title == "Parallel Decoder")
            .expect("parallel decoder should expose sampling points");
        let enable = binary_sampling
            .overlay
            .activities
            .first()
            .expect("parallel decoder should publish its derived enable activity");
        assert!(enable.is_active_at(800_000_000));
        assert!(!enable.is_active_at(1_200_000_000));
    }

    #[test]
    fn binary_sampling_activity_reaches_the_ui_candidate_after_a_run() {
        let widget = binary_decoder_demo_widget();
        let mut ctx = CompileCtx::default();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx).unwrap();
        let overlays = ctx.take_sampling_overlays();
        let binary_sampling = overlays
            .iter()
            .find(|candidate| candidate.node_title == "Parallel Decoder")
            .expect("parallel decoder should expose sampling points");
        let enable = binary_sampling
            .overlay
            .activities
            .first()
            .expect("parallel decoder should publish its derived enable activity");

        run.wait();

        assert!(enable.is_active_at(800_000_000));
        assert!(!enable.is_active_at(1_200_000_000));
        let completed_enable = run
            .sampling_overlays()
            .iter()
            .find(|candidate| candidate.node_title == "Parallel Decoder")
            .and_then(|candidate| candidate.overlay.activities.first())
            .expect("completed run should retain the parallel enable activity");
        assert!(completed_enable.is_active_at(800_000_000));
        assert!(!completed_enable.is_active_at(1_200_000_000));
    }

    #[test]
    fn binary_decoder_demo_latch_follows_every_start_stop_pair() {
        let widget = binary_decoder_demo_widget();
        let mut ctx = CompileCtx::default();
        let lanes = ctx.derived_lanes.clone();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx).unwrap();
        run.wait();

        let lanes = lanes.read();
        let q = lanes
            .iter()
            .find(|lane| lane.name == "SR Flip-Flop.Q")
            .expect("latch output should be visible");
        let signal_processing::DerivedLaneData::Digital(samples) = &q.data else {
            panic!("latch output should be a digital lane");
        };
        assert_eq!(samples.len(), 25);
        assert!(
            samples
                .iter()
                .enumerate()
                .all(|(index, sample)| sample.value == !index.is_multiple_of(2))
        );
        assert!(
            samples
                .windows(2)
                .all(|pair| pair[0].start_time_ns <= pair[1].start_time_ns)
        );
    }

    #[test]
    fn uart_viewer_tracks_carry_explicit_presentation_metadata() {
        let compiled = lower(uart_demo_widget().graph(), &BuilderRegistry::standard()).unwrap();
        let viewer = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .unwrap();
        let mut tracks = viewer
            .resolved
            .members(0)
            .into_iter()
            .filter_map(|(_, input)| {
                input
                    .viewer_presentation
                    .as_ref()
                    .map(|presentation| presentation.track_key.as_str())
            })
            .collect::<Vec<_>>();
        tracks.sort_unstable();

        // The demo connects only Data. Explicit grouping still produces a
        // valid partial compound group rather than relying on a Bits lane
        // being present or discoverable by name.
        assert_eq!(tracks, ["frame"]);
    }

    #[test]
    fn spi_viewer_tracks_form_explicit_mosi_and_miso_groups() {
        let widget = binary_decoder_demo_widget();
        let spi = node_by_def(&widget, "SPI Decoder");
        let compiled = lower(widget.graph(), &BuilderRegistry::standard()).unwrap();
        let viewer = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .unwrap();
        let mut tracks = viewer
            .resolved
            .members(0)
            .into_iter()
            .filter(|(_, input)| input.source_node == spi)
            .filter_map(|(_, input)| {
                input.viewer_presentation.as_ref().map(|presentation| {
                    (
                        presentation.group_key.as_str(),
                        presentation.track_key.as_str(),
                    )
                })
            })
            .collect::<Vec<_>>();
        tracks.sort_unstable();

        assert_eq!(
            tracks,
            [
                ("miso", "bits"),
                ("miso", "data"),
                ("mosi", "bits"),
                ("mosi", "data"),
            ]
        );
    }

    #[test]
    fn duplicate_and_renamed_decoders_keep_distinct_explicit_groups() {
        let mut widget = uart_demo_widget();
        let source = node_by_def(&widget, "Test UART Source");
        let first_decoder = node_by_def(&widget, "UART Decoder");
        let viewer = node_by_def(&widget, "Viewer");
        let second_decoder = widget
            .add_node_at(nodes::UartDecoder::name(), Pos2::new(420.0, 420.0))
            .unwrap();
        for decoder in [first_decoder, second_decoder] {
            widget.graph_mut().nodes.get_mut(&decoder).unwrap().title = "Duplicate title".into();
        }
        let connect = |widget: &mut NodeGraphWidget, from: (NodeId, &str), to: (NodeId, &str)| {
            let from_index = output_index(widget, from.0, from.1);
            let to_index = input_index(widget, to.0, to.1);
            widget.graph_mut().add_connection(
                SocketId {
                    node: from.0,
                    index: from_index,
                    direction: SocketDirection::Output,
                },
                SocketId {
                    node: to.0,
                    index: to_index,
                    direction: SocketDirection::Input,
                },
            );
        };
        connect(&mut widget, (source, "RX"), (second_decoder, "RX/TX"));
        connect(&mut widget, (second_decoder, "Data"), (viewer, "In"));

        let build_groups = |widget: &NodeGraphWidget| {
            let builders = BuilderRegistry::standard();
            let compiled = lower(widget.graph(), &builders).unwrap();
            let viewer = compiled
                .nodes
                .iter()
                .find(|node| node.builder == "Viewer")
                .unwrap();
            let ctx = CompileCtx::default();
            register_collected_subscribers(
                viewer,
                builders.get("Viewer").unwrap(),
                &viewer.runtime_name,
                &ctx,
            )
            .unwrap();
            let groups = ctx.waveform_presentations.read();
            groups
                .iter()
                .filter(|group| {
                    group
                        .tracks
                        .iter()
                        .any(|track| track.id.as_str() == "frame")
                })
                .map(|group| (group.id.as_str().to_owned(), group.label.clone()))
                .collect::<Vec<_>>()
        };

        let before = build_groups(&widget);
        assert_eq!(before.len(), 2);
        assert_ne!(before[0].0, before[1].0);
        assert!(before.iter().all(|(_, label)| label == "Duplicate title"));

        widget
            .graph_mut()
            .nodes
            .get_mut(&first_decoder)
            .unwrap()
            .title = "Renamed decoder".into();
        let after = build_groups(&widget);
        assert_eq!(
            before.iter().map(|(id, _)| id).collect::<Vec<_>>(),
            after.iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
        assert!(after.iter().any(|(_, label)| label == "Renamed decoder"));
    }

    #[test]
    fn plugin_builder_can_contribute_a_lane_renderer() {
        use std::sync::Arc;

        use logic_analyzer_viewer::{
            DefaultViewerLaneRenderer, ViewerLaneBadge, ViewerLaneRenderer,
            ViewerOutputPresentation,
        };

        struct PluginBuilder;
        impl RuntimeBuilder for PluginBuilder {
            fn accepted_kinds(&self, _: &Socket, _: &Value) -> Vec<PortKind> {
                Vec::new()
            }

            fn offered_kinds(&self, _: &Socket, _: &Value) -> Vec<PortKind> {
                Vec::new()
            }

            fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
                None
            }

            fn output_port(&self, _: &Socket, _: &Value, _: PortKind) -> Option<String> {
                None
            }

            fn viewer_output_presentation(
                &self,
                _: &Socket,
                _: &Value,
            ) -> Option<ViewerOutputPresentation> {
                let renderer: Arc<dyn ViewerLaneRenderer> = Arc::new(DefaultViewerLaneRenderer);
                Some(ViewerOutputPresentation::new(
                    "plugin group",
                    "plugin track",
                    0,
                    1.0,
                    ViewerLaneBadge::new("P", Color32::WHITE),
                    renderer,
                ))
            }

            fn build(
                &self,
                _: &str,
                _: &Value,
                _: &ResolvedInputs,
                _: &mut CompileCtx,
            ) -> Result<Box<dyn ProcessNode>, String> {
                Err("not needed by presentation registration test".into())
            }
        }

        let mut node_types = nodes::build_registry();
        let mut builders = BuilderRegistry::standard();
        crate::PluginContext::new(&mut node_types, &mut builders)
            .register_builder("Plugin Presenter", Box::new(PluginBuilder));
        let widget = uart_demo_widget();
        let socket = &widget
            .graph()
            .nodes
            .values()
            .find(|node| node.def_name() == "UART Decoder")
            .unwrap()
            .outputs[3];
        let presentation = builders
            .get("Plugin Presenter")
            .unwrap()
            .viewer_output_presentation(socket, &Value::Null)
            .unwrap();

        assert_eq!(presentation.group_key, "plugin group");
        assert_eq!(presentation.track_key, "plugin track");
    }

    #[test]
    fn buffered_provider_registers_through_the_existing_live_feature_contract() {
        let mut node_types = nodes::build_registry();
        let mut builders = BuilderRegistry::standard();
        crate::PluginContext::new(&mut node_types, &mut builders).register_builder(
            nodes::TestCaptureSource::name(),
            Box::new(BufferedPluginBuilder),
        );
        let mut widget = NodeGraphWidget::new(node_types);
        let source = widget
            .add_node_at(nodes::TestCaptureSource::name(), Pos2::ZERO)
            .unwrap();

        let feature = discover_live_capture_feature(widget.graph(), &builders)
            .unwrap()
            .expect("registered builder should expose its live feature");

        assert_eq!(feature.source_node, source);
        assert_eq!(
            feature.capabilities().data_delivery(),
            CaptureDataDelivery::BufferedUpload
        );
        assert_eq!(
            feature.channels(),
            [
                CaptureChannelId::new("pod-a:3"),
                CaptureChannelId::new("pod-q:41"),
                CaptureChannelId::new("aux-bank:9"),
            ]
        );
        assert_eq!(feature.capabilities().setting_matrix().len(), 2);
        assert!(
            feature
                .capabilities()
                .supports(feature.channels(), feature.sample_rate_hz())
        );
        assert!(!feature.capabilities().supports_force_trigger());
    }

    #[test]
    fn advanced_trigger_program_routes_unchanged_to_the_registered_builder() {
        let mut node_types = nodes::build_registry();
        let mut builders = BuilderRegistry::standard();
        crate::PluginContext::new(&mut node_types, &mut builders).register_builder(
            nodes::TestCaptureSource::name(),
            Box::new(BufferedPluginBuilder),
        );
        let mut widget = NodeGraphWidget::new(node_types);
        let source = widget
            .add_node_at(nodes::TestCaptureSource::name(), Pos2::ZERO)
            .unwrap();
        let program = TriggerProgram::new(
            TriggerIdentifier::new("plugin.vendor-neutral.engine").unwrap(),
            17,
            vec![TriggerStage {
                predicates: vec![TriggerPredicate::Digital {
                    channel: CaptureChannelId::new("pod-q:41"),
                    condition: SimpleTriggerCondition::Falling,
                }],
                logic: TriggerLogicOperator::And,
                inverted: false,
                count: None,
            }],
        );

        let state = apply_live_capture_edit(
            widget.graph(),
            &builders,
            source,
            &LiveCaptureEdit::SetTriggerProgram {
                program: Some(program.clone()),
            },
        )
        .unwrap();

        assert_eq!(
            state["received_program"],
            serde_json::to_value(Some(program)).unwrap()
        );
        assert_eq!(state["previous_state"], widget.graph().nodes[&source].state);
    }

    #[test]
    fn file_source_bounds_exact_derived_data_entries() {
        let widget = startup_widget();
        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));

        assert_eq!(
            compiled.derived_data_retention,
            DerivedDataRetention::MaxEntries(signal_processing::DEFAULT_DERIVED_DATA_MAX_ENTRIES)
        );
    }

    #[test]
    fn missing_writer_input_is_reported() {
        let mut widget = startup_widget();
        // Cut the filename wire; the writer input becomes a compile error.
        let graph = widget.graph_mut();
        let writer = graph
            .nodes
            .values()
            .find(|n| n.def_name() == "File Writer")
            .unwrap()
            .id;
        let index = graph
            .connections
            .iter()
            .position(|c| c.to.node == writer && c.to.index == 1)
            .unwrap();
        graph.remove_connection_at(index);

        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.node == Some(writer) && e.message.contains("Filename")),
            "expected filename error, got {errors:?}"
        );
    }

    /// A wired File socket on the DSL File Source builds the deferred
    /// variant (filename arrives at run start over the wire); unconnected
    /// keeps the build-time open, and unconnected + empty picker is
    /// required (a compile error).
    #[test]
    fn file_source_with_wired_filename_builds_deferred_source() {
        use signal_processing::TextSample;

        use crate::nodes::sources::FileSourceBuilder;

        let builder = FileSourceBuilder;
        let state = serde_json::to_value(nodes::DslFileSourceState {
            file: node_graph::FileValue::new(""),
            channels: node_graph::IntValue::new(4, 1, 32),
        })
        .unwrap();

        let file_socket = Socket {
            name: "File".into(),
            type_name: "Text".into(),
            color: egui::Color32::WHITE,
            shape: node_graph::SocketShape::Circle,
            allowed: vec![],
            resolved_type: None,
            def_index: 0,
            variadic: None,
            visible: true,
            editor_visible: true,
            hidden: false,
            has_control: true,
            view_selectable: false,
            view_indicator_sources: Vec::new(),
            show_in_view: false,
        };
        assert_eq!(
            builder.accepted_kinds(&file_socket, &state),
            vec![PortKind::of::<TextSample>()],
            "the File socket accepts a Text filename wire"
        );
        assert!(!builder.input_required(&file_socket, &state));

        let mut resolved = ResolvedInputs::default();
        resolved.0.insert(
            (0, 0),
            ResolvedInput {
                kind: PortKind::of::<TextSample>(),
                source: "Formatter.Text".into(),
                source_node: NodeId(1),
                source_node_title: "Formatter".into(),
                word_display_format: None,
                viewer_presentation: None,
                decoder_table_column: None,
                capture_channel: None,
            },
        );
        let node = builder
            .build("src", &state, &resolved, &mut CompileCtx::default())
            .expect("wired filename must not require the file to exist at build");
        assert_eq!(
            node.num_inputs(),
            1,
            "expected the deferred source (one filename input)"
        );
    }

    /// The counterpart to `missing_writer_input_is_reported`: with the
    /// writer's static filename (save-dialog prop) set, an unconnected
    /// Filename input is fine — the graph compiles and the writer is built
    /// with the static path.
    #[test]
    fn static_filename_makes_writer_filename_input_optional() {
        let mut widget = startup_widget();
        let graph = widget.graph_mut();
        let writer = graph
            .nodes
            .values()
            .find(|n| n.def_name() == "File Writer")
            .unwrap()
            .id;
        let index = graph
            .connections
            .iter()
            .position(|c| c.to.node == writer && c.to.index == 1)
            .unwrap();
        graph.remove_connection_at(index);

        let mut state: nodes::FileWriterState =
            serde_json::from_value(graph.nodes[&writer].state.clone()).unwrap();
        state.filename = node_graph::FileValue::new_save("/tmp/capture.bin", "Save capture as");
        widget.set_node_state(writer, serde_json::to_value(state).unwrap());

        lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("expected the graph to compile: {errors:?}"));
    }

    #[test]
    fn buffer_node_kind_mismatch_is_rejected() {
        use egui::Pos2;
        use node_graph::{NodeDef, SocketDirection, SocketId};

        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at(nodes::TestUartSource::name(), Pos2::new(0.0, 0.0))
            .unwrap();
        let buf = widget
            .add_node_at(nodes::Buffer::name(), Pos2::new(200.0, 0.0))
            .unwrap();
        let viewer = widget
            .add_node_at(nodes::Viewer::name(), Pos2::new(400.0, 0.0))
            .unwrap();

        // TestUartSource offers `Sample` ("Signal"); set the buffer to
        // "Trigger" — no common kind on the source -> buffer edge, must be
        // a compile error (regardless of what the buffer -> viewer edge
        // downstream negotiates to).
        let mut state = nodes::Buffer::state();
        state.kind.select("Trigger");
        widget.set_node_state(buf, serde_json::to_value(state).unwrap());

        let connect = |widget: &mut NodeGraphWidget, from: (NodeId, &str), to: (NodeId, &str)| {
            let from_socket = SocketId {
                node: from.0,
                index: output_index(widget, from.0, from.1),
                direction: SocketDirection::Output,
            };
            let to_socket = SocketId {
                node: to.0,
                index: input_index(widget, to.0, to.1),
                direction: SocketDirection::Input,
            };
            widget.graph_mut().add_connection(from_socket, to_socket);
        };
        connect(&mut widget, (source, "RX"), (buf, "In"));
        // A dangling output is unreachable and gets pruned before kind
        // negotiation runs — give the buffer a sink so it stays reachable
        // and the mismatch on its input actually gets checked.
        connect(&mut widget, (buf, "Out"), (viewer, "In"));

        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(
            errors.iter().any(|e| e.node == Some(buf)),
            "expected a compile error on the buffer node, got {errors:?}"
        );
    }

    #[test]
    fn muted_node_with_compatible_pass_through_lowers_to_a_direct_connection() {
        use egui::Pos2;
        use node_graph::{NodeDef, SocketDirection, SocketId};

        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at(nodes::TestUartSource::name(), Pos2::new(0.0, 0.0))
            .unwrap();
        let buf = widget
            .add_node_at(nodes::Buffer::name(), Pos2::new(200.0, 0.0))
            .unwrap();
        let viewer = widget
            .add_node_at(nodes::Viewer::name(), Pos2::new(400.0, 0.0))
            .unwrap();

        let connect = |widget: &mut NodeGraphWidget, from: (NodeId, &str), to: (NodeId, &str)| {
            let from_socket = SocketId {
                node: from.0,
                index: output_index(widget, from.0, from.1),
                direction: SocketDirection::Output,
            };
            let to_socket = SocketId {
                node: to.0,
                index: input_index(widget, to.0, to.1),
                direction: SocketDirection::Input,
            };
            widget.graph_mut().add_connection(from_socket, to_socket);
        };
        connect(&mut widget, (source, "RX"), (buf, "In"));
        connect(&mut widget, (buf, "Out"), (viewer, "In"));
        widget.graph_mut().nodes.get_mut(&buf).unwrap().muted = true;

        let compiled =
            lower(widget.graph(), &BuilderRegistry::standard()).unwrap_or_else(|errors| {
                panic!("expected the muted buffer to splice through: {errors:?}")
            });

        assert!(
            compiled.nodes.iter().all(|n| n.id != buf),
            "muted node must be dropped from the compiled graph, got {:?}",
            compiled.nodes
        );
        assert_eq!(compiled.edges.len(), 1);
        let edge = &compiled.edges[0];
        assert_eq!(edge.from.0, source);
        assert_eq!(edge.to.0, viewer);
    }

    #[test]
    fn muted_node_without_compatible_pass_through_reports_a_targeted_error() {
        use egui::Pos2;
        use node_graph::{SocketDirection, SocketId};

        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at("Test UART Source", Pos2::new(0.0, 0.0))
            .unwrap();
        let matcher = widget
            .add_node_at("Word Matcher", Pos2::new(200.0, 0.0))
            .unwrap();
        let flip_flop = widget
            .add_node_at("SR Flip-Flop", Pos2::new(400.0, 0.0))
            .unwrap();
        let viewer = widget.add_node_at("Viewer", Pos2::new(600.0, 0.0)).unwrap();

        let connect = |widget: &mut NodeGraphWidget, from: (NodeId, &str), to: (NodeId, &str)| {
            let from_socket = SocketId {
                node: from.0,
                index: output_index(widget, from.0, from.1),
                direction: SocketDirection::Output,
            };
            let to_socket = SocketId {
                node: to.0,
                index: input_index(widget, to.0, to.1),
                direction: SocketDirection::Input,
            };
            widget.graph_mut().add_connection(from_socket, to_socket);
        };
        // Word Matcher's only input is `Words`-typed and its outputs are
        // `Trigger`/`Signal` — none of those pairs share a type, so it has
        // no pass-through no matter what's wired to it. Connecting it from
        // the Signal-typed source (bypassing the editor's own connect-time
        // type check, as `buffer_node_kind_mismatch_is_rejected` does above)
        // just gives it something realistic to break.
        connect(&mut widget, (source, "RX"), (matcher, "Words"));
        connect(&mut widget, (matcher, "Match"), (flip_flop, "Set"));
        connect(&mut widget, (flip_flop, "Q"), (viewer, "In"));
        widget.graph_mut().nodes.get_mut(&matcher).unwrap().muted = true;

        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.node == Some(matcher) && e.message.contains("Muted")),
            "expected a targeted error on the muted Word Matcher, got {errors:?}"
        );
    }

    #[test]
    fn muted_source_reports_the_break_and_prunes_its_branch() {
        // A source has no data input at all — no config property shares
        // its output's type either — so it can never have a pass-through
        // pair. Muting it is a hard break, not a silent no-op: the targeted
        // error should point at the source, and its downstream branch
        // should vanish from the compiled graph rather than dangling.
        let mut widget = uart_demo_widget();
        let source = node_by_def(&widget, "Test UART Source");
        widget.graph_mut().nodes.get_mut(&source).unwrap().muted = true;

        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.node == Some(source) && e.message.contains("Muted")),
            "expected a targeted error on the muted source, got {errors:?}"
        );
    }

    fn output_index(widget: &NodeGraphWidget, node: NodeId, name: &str) -> usize {
        widget.graph().nodes[&node]
            .outputs
            .iter()
            .position(|socket| socket.name == name)
            .unwrap_or_else(|| panic!("no output socket '{name}'"))
    }

    fn input_index(widget: &NodeGraphWidget, node: NodeId, name: &str) -> usize {
        widget.graph().nodes[&node]
            .inputs
            .iter()
            .position(|socket| socket.name == name && socket.visible)
            .unwrap_or_else(|| panic!("no input socket '{name}'"))
    }

    fn node_by_def(widget: &NodeGraphWidget, def: &str) -> NodeId {
        widget
            .graph()
            .nodes
            .values()
            .find(|node| node.def_name() == def)
            .unwrap_or_else(|| panic!("no '{def}' node"))
            .id
    }

    // ── diff classification ───────────────────────────────────────────

    #[test]
    fn diff_classifies_matcher_pattern_change_as_hot_config() {
        let registry = BuilderRegistry::standard();
        let mut widget = startup_widget();
        let old = lower(widget.graph(), &registry).unwrap();

        let matcher = widget
            .graph()
            .nodes
            .values()
            .find(|node| node.title == "Match Start")
            .unwrap()
            .id;
        let mut state: nodes::WordMatcherState =
            serde_json::from_value(widget.graph().nodes[&matcher].state.clone()).unwrap();
        state.pattern = node_graph::StringValue::new("0x600082");
        widget.set_node_state(matcher, serde_json::to_value(state).unwrap());

        let new = lower(widget.graph(), &registry).unwrap();
        let edits = diff(&old, &new, &registry).unwrap();
        assert_eq!(edits.len(), 1);
        match &edits[0] {
            LiveEdit::Configure(id, config) => {
                assert_eq!(*id, matcher);
                assert_eq!(config.get("pattern"), Some(&ConfigValue::U64(0x600082)));
            }
            other => panic!("expected Configure, got {other:?}"),
        }
    }

    #[test]
    fn diff_rejects_source_fed_restart() {
        let registry = BuilderRegistry::standard();
        let mut widget = startup_widget();
        let old = lower(widget.graph(), &registry).unwrap();

        // SPI word size has no hot config and the decoder is source-fed.
        let spi = node_by_def(&widget, "SPI Decoder");
        let mut state: nodes::SpiDecoderState =
            serde_json::from_value(widget.graph().nodes[&spi].state.clone()).unwrap();
        state.word_size = node_graph::IntValue::new(16, 1, 32);
        widget.set_node_state(spi, serde_json::to_value(state).unwrap());

        let new = lower(widget.graph(), &registry).unwrap();
        let error = diff(&old, &new, &registry).unwrap_err();
        assert!(error.contains("fed directly by the source"), "{error}");
    }

    /// Wires a new matcher onto the **binary decoder's** words — the one
    /// event branch that stays live for the whole run. The SPI control
    /// branch is index-driven (EdgeQuery) and decodes the entire capture
    /// in seconds, long before the block-streaming path produces its first
    /// capture file — a tap attached to it mid-run would join an
    /// already-closed stream and correctly observe nothing (event streams
    /// don't replay). Mask 0x0 matches every word, so the tap fires as
    /// soon as any enabled window streams data.
    fn attach_matcher_tap(widget: &mut NodeGraphWidget) -> NodeId {
        let matcher = widget
            .add_node_at("Word Matcher", egui::Pos2::new(620.0, 600.0))
            .unwrap();
        let mut state: nodes::WordMatcherState =
            serde_json::from_value(widget.graph().nodes[&matcher].state.clone()).unwrap();
        state.pattern = node_graph::StringValue::new("0x0");
        state.mask = node_graph::StringValue::new("0x0");
        widget.set_node_state(matcher, serde_json::to_value(state).unwrap());

        let decoder = node_by_def(widget, "Binary Decoder");
        let out_idx = |graph: &node_graph::GraphState, id: NodeId, name: &str| {
            graph.nodes[&id]
                .outputs
                .iter()
                .position(|s| s.name == name)
                .unwrap()
        };
        let input_idx = |graph: &node_graph::GraphState, id: NodeId, name: &str| {
            graph.nodes[&id]
                .inputs
                .iter()
                .position(|s| s.name == name && s.visible)
                .unwrap()
        };
        let graph = widget.graph_mut();
        let decoder_words = out_idx(graph, decoder, "Words");
        let matcher_in = input_idx(graph, matcher, "Words");
        graph.add_connection(
            SocketId {
                node: decoder,
                index: decoder_words,
                direction: node_graph::SocketDirection::Output,
            },
            SocketId {
                node: matcher,
                index: matcher_in,
                direction: node_graph::SocketDirection::Input,
            },
        );
        let matcher_out = out_idx(graph, matcher, "Match");
        graph.nodes.get_mut(&matcher).unwrap().outputs[matcher_out].show_in_view = true;
        matcher
    }

    #[test]
    fn diff_classifies_tap_attach_as_add_plus_viewer_restart() {
        let registry = BuilderRegistry::standard();
        let mut widget = startup_widget();
        let old = lower(widget.graph(), &registry).unwrap();

        let matcher = attach_matcher_tap(&mut widget);
        let new = lower(widget.graph(), &registry).unwrap();
        let edits = diff(&old, &new, &registry).unwrap();

        assert!(
            edits
                .iter()
                .any(|edit| matches!(edit, LiveEdit::Add(id) if *id == matcher)),
            "{edits:?}"
        );
        assert!(
            edits
                .iter()
                .any(|edit| matches!(edit, LiveEdit::Restart(id) if *id == AUTO_VIEW_NODE_ID)),
            "{edits:?}"
        );
        assert_eq!(edits.len(), 2, "{edits:?}");
    }

    #[test]
    fn diff_ignores_a_legacy_source_view_flag() {
        let registry = BuilderRegistry::standard();
        let mut widget = startup_widget();
        let old = lower(widget.graph(), &registry).unwrap();

        let source = node_by_def(&widget, "DSL File Source");
        widget.graph_mut().nodes.get_mut(&source).unwrap().outputs[9].show_in_view = true;

        let new = lower(widget.graph(), &registry).unwrap();
        assert!(diff(&old, &new, &registry).unwrap().is_empty());
    }

    fn repo_path(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
    }

    /// Reference pipeline: the byte-exact Phase-1 wiring of
    /// `examples/spi_graph_decode.rs`.
    fn run_reference(capture: &Path, out_dir: &Path) {
        use logic_analyzer_processing::nodes::decoders::parallel_decoder::{
            ParallelDecoder, StrobeMode,
        };
        use logic_analyzer_processing::nodes::decoders::spi_decoder::{SpiDecoder, SpiMode};
        use logic_analyzer_processing::nodes::logic::sr_latch::SrLatch;
        use logic_analyzer_processing::nodes::logic::text_formatter::TextFormatter;
        use logic_analyzer_processing::nodes::logic::trigger_counter::TriggerCounter;
        use logic_analyzer_processing::nodes::logic::word_matcher::WordMatcher;
        use logic_analyzer_processing::nodes::sources::dsl_file::DslFileSource;
        use logic_analyzer_processing::types::CsPolarity;

        let mut pipeline = Pipeline::new().with_default_buffer_size(10_000_000);
        pipeline
            .add_process("source", DslFileSource::new(capture, 11).unwrap())
            .unwrap();
        pipeline
            .add_process("spi", SpiDecoder::new(SpiMode::Mode0, 24, true, false))
            .unwrap();
        pipeline
            .add_process("start", WordMatcher::new(0x600081, u64::MAX))
            .unwrap();
        pipeline
            .add_process("stop", WordMatcher::new(0x600000, u64::MAX))
            .unwrap();
        pipeline.add_process("latch", SrLatch::new(false)).unwrap();
        pipeline
            .add_process("counter", TriggerCounter::new(0, 1))
            .unwrap();
        pipeline
            .add_process(
                "formatter",
                TextFormatter::new(format!("{}/capture_{{n:04}}.bin", out_dir.display())),
            )
            .unwrap();
        pipeline
            .add_process(
                "decoder",
                ParallelDecoder::new(8, StrobeMode::AnyEdge, CsPolarity::ActiveLow),
            )
            .unwrap();
        pipeline
            .add_process("writer", BinaryFileWriter::new().with_index_csv(true))
            .unwrap();

        pipeline.connect("source", "ch7", "spi", "clk").unwrap();
        pipeline.connect("source", "ch8", "spi", "cs").unwrap();
        pipeline.connect("source", "ch6", "spi", "mosi").unwrap();
        pipeline
            .connect_with_buffer("spi", "mosi_words", "start", "words", 1_000)
            .unwrap();
        pipeline
            .connect_with_buffer("spi", "mosi_words", "stop", "words", 1_000)
            .unwrap();
        pipeline
            .connect_with_buffer("start", "trigger", "latch", "set", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("stop", "trigger", "latch", "reset", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("latch", "q", "decoder", "enable_signal", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("start", "trigger", "counter", "trigger", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("counter", "count", "formatter", "value", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("formatter", "text", "writer", "filename", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("source", "ch10", "decoder", "strobe", 4)
            .unwrap();
        for bit in 0..8 {
            pipeline
                .connect_with_buffer(
                    "source",
                    &format!("ch{bit}"),
                    "decoder",
                    &format!("d{bit}"),
                    4,
                )
                .unwrap();
        }
        // Same channel 8 as `spi.cs` above, negotiated onto a *different*
        // SampleKind (Block, not Edge) for this destination — the mixed-
        // kind fan-out this whole change exists to collapse into one port.
        pipeline
            .connect_with_buffer("source", "ch8", "decoder", "cs", 4)
            .unwrap();
        pipeline
            .connect_with_buffer("decoder", "words", "writer", "data", 100_000)
            .unwrap();

        pipeline.build().unwrap().wait();
    }

    fn run_current_reference(capture: &Path, out_dir: &Path) {
        use logic_analyzer_processing::nodes::decoders::parallel_decoder::{
            ParallelDecoder, StrobeMode,
        };
        use logic_analyzer_processing::nodes::decoders::spi_decoder::{SpiDecoder, SpiMode};
        use logic_analyzer_processing::nodes::logic::logic_gate::{GateOp, LogicGate};
        use logic_analyzer_processing::nodes::logic::sr_latch::SrLatch;
        use logic_analyzer_processing::nodes::logic::text_formatter::TextFormatter;
        use logic_analyzer_processing::nodes::logic::trigger_counter::TriggerCounter;
        use logic_analyzer_processing::nodes::logic::word_matcher::WordMatcher;
        use logic_analyzer_processing::nodes::sources::dsl_file::DslFileSource;
        use logic_analyzer_processing::types::CsPolarity;

        let mut pipeline = Pipeline::new().with_default_buffer_size(10_000_000);
        pipeline
            .add_process("source", DslFileSource::new(capture, 11).unwrap())
            .unwrap();
        pipeline
            .add_process("spi", SpiDecoder::new(SpiMode::Mode0, 24, true, false))
            .unwrap();
        pipeline
            .add_process("start", WordMatcher::new(0x600081, u64::MAX))
            .unwrap();
        pipeline
            .add_process("stop", WordMatcher::new(0x600000, u64::MAX))
            .unwrap();
        pipeline.add_process("latch", SrLatch::new(false)).unwrap();
        pipeline
            .add_process("gate", LogicGate::new(GateOp::And, 2))
            .unwrap();
        pipeline
            .add_process("counter", TriggerCounter::new(0, 1))
            .unwrap();
        pipeline
            .add_process(
                "formatter",
                TextFormatter::new(format!("{}/capture_{{n:04}}.bin", out_dir.display())),
            )
            .unwrap();
        pipeline
            .add_process(
                "decoder",
                ParallelDecoder::new(8, StrobeMode::AnyEdge, CsPolarity::Disabled),
            )
            .unwrap();
        pipeline
            .add_process("writer", BinaryFileWriter::new().with_index_csv(true))
            .unwrap();

        pipeline.connect("source", "ch7", "spi", "clk").unwrap();
        pipeline.connect("source", "ch8", "spi", "cs").unwrap();
        pipeline.connect("source", "ch6", "spi", "mosi").unwrap();
        pipeline
            .connect("spi", "mosi_words", "start", "words")
            .unwrap();
        pipeline
            .connect("spi", "mosi_words", "stop", "words")
            .unwrap();
        pipeline
            .connect("start", "trigger", "latch", "set")
            .unwrap();
        pipeline
            .connect("stop", "trigger", "latch", "reset")
            .unwrap();
        pipeline.connect("source", "ch8", "gate", "in0").unwrap();
        pipeline.connect("latch", "q", "gate", "in1").unwrap();
        pipeline
            .connect("gate", "out", "decoder", "enable_signal")
            .unwrap();
        pipeline
            .connect("start", "trigger", "counter", "trigger")
            .unwrap();
        pipeline
            .connect("counter", "count", "formatter", "value")
            .unwrap();
        pipeline
            .connect("formatter", "text", "writer", "filename")
            .unwrap();
        pipeline
            .connect("source", "ch10", "decoder", "strobe")
            .unwrap();
        for bit in 0..8 {
            pipeline
                .connect("source", &format!("ch{bit}"), "decoder", &format!("d{bit}"))
                .unwrap();
        }
        pipeline
            .connect("decoder", "words", "writer", "data")
            .unwrap();
        pipeline.build().unwrap().wait();
    }

    fn bin_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().into_string().unwrap();
                (name.starts_with("capture_") && name.ends_with(".bin")).then_some(name)
            })
            .collect();
        names.sort();
        names
    }

    /// captures.csv rows with the filename column reduced to its basename,
    /// so runs into different directories compare equal.
    fn normalized_csv(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("captures.csv"))
            .unwrap()
            .lines()
            .map(|line| {
                line.split(',')
                    .map(|field| field.rsplit('/').next().unwrap_or(field))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect()
    }

    /// Startup graph pointed at `capture` with the writer template in
    /// `out_dir`.
    fn golden_widget(capture: &Path, out_dir: &Path) -> NodeGraphWidget {
        let mut widget = startup_widget();
        let source_id = node_by_def(&widget, "DSL File Source");
        let formatter_id = node_by_def(&widget, "String Formatter");
        widget.set_node_state(
            source_id,
            serde_json::to_value(nodes::DslFileSourceState {
                file: node_graph::FileValue::new(capture.display().to_string()),
                channels: node_graph::IntValue::new(11, 1, 32),
            })
            .unwrap(),
        );
        widget.set_node_state(
            formatter_id,
            serde_json::to_value(nodes::StringFormatterState {
                template: node_graph::StringValue::new(format!(
                    "{}/capture_{{n:04}}.bin",
                    out_dir.display()
                )),
            })
            .unwrap(),
        );
        widget
    }

    /// The live-tap gate: attach a matcher tap mid-run and detach it
    /// again; the untouched writer branch must produce byte-identical
    /// output to an uninterrupted reference run, and the tap must actually
    /// have collected data while attached.
    #[test]
    #[ignore = "runs the full wipneus5.dsl capture; use --release"]
    fn live_attach_detach_preserves_writer_output() {
        let capture = repo_path("_captures/wipneus5.dsl");
        assert!(capture.exists(), "capture not found: {}", capture.display());

        let tmp = tempfile::tempdir().unwrap();
        let graph_dir = tmp.path().join("graph");
        let ref_dir = tmp.path().join("reference");
        std::fs::create_dir_all(&graph_dir).unwrap();
        std::fs::create_dir_all(&ref_dir).unwrap();

        // The reference pipeline is a second, entirely independent full pass
        // over the same multi-billion-sample capture (own process, own
        // output dir) — nothing about it depends on the live-graph run
        // below, so it runs concurrently on its own thread instead of
        // afterward, roughly halving this test's wall-clock time on a
        // machine with room for both.
        let reference_handle = {
            let capture = capture.clone();
            let ref_dir = ref_dir.clone();
            std::thread::spawn(move || run_reference(&capture, &ref_dir))
        };

        let registry = BuilderRegistry::standard();
        let mut widget = golden_widget(&capture, &graph_dir);
        let mut ctx = CompileCtx::default();
        let lanes = ctx.derived_lanes.clone();
        let mut run = start_live(widget.graph(), &registry, &mut ctx)
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));

        // Wait until the pipeline demonstrably produces output, then attach.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        while bin_files(&graph_dir).is_empty() {
            assert!(!run.is_finished(), "run finished before any capture file");
            assert!(
                std::time::Instant::now() < deadline,
                "no capture file within deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        let matcher = attach_matcher_tap(&mut widget);
        let summary = run.apply(widget.graph(), &registry).expect("attach tap");
        assert_eq!(summary.added, 1, "{summary:?}");
        assert_eq!(summary.restarted, 1, "{summary:?}"); // viewer rewired

        // Let the tap observe at least one window, then detach it — poll
        // instead of a fixed sleep so this only takes as long as it
        // actually needs to.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            let observed = lanes.read().iter().any(|lane| {
                lane.name.contains("Word Matcher.Match")
                    && matches!(&lane.data, signal_processing::DerivedLaneData::Markers(markers) if !markers.is_empty())
            });
            if observed {
                break;
            }
            assert!(
                !run.is_finished(),
                "run finished before the tap observed anything"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "tap never observed a trigger within deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        widget.graph_mut().remove_node(matcher);
        let summary = run.apply(widget.graph(), &registry).expect("detach tap");
        assert_eq!(summary.removed, 1, "{summary:?}");
        assert_eq!(summary.restarted, 1, "{summary:?}");

        run.wait();
        reference_handle.join().expect("reference run panicked");

        // The writer branch never noticed any of it.
        let graph_files = bin_files(&graph_dir);
        let ref_files = bin_files(&ref_dir);
        assert!(!ref_files.is_empty());
        assert_eq!(graph_files, ref_files, "different file sets");
        for name in &ref_files {
            let a = std::fs::read(graph_dir.join(name)).unwrap();
            let b = std::fs::read(ref_dir.join(name)).unwrap();
            assert_eq!(a, b, "{name} differs");
        }
        assert_eq!(normalized_csv(&graph_dir), normalized_csv(&ref_dir));

        // The tap collected triggers while attached.
        let lanes = lanes.read();
        let tap_lane = lanes
            .iter()
            .find(|lane| lane.name.contains("Word Matcher.Match"))
            .expect("tap lane registered");
        match &tap_lane.data {
            signal_processing::DerivedLaneData::Markers(markers) => {
                assert!(!markers.is_empty(), "tap never fired while attached");
            }
            other => panic!("expected marker lane, got {other:?}"),
        }
    }

    /// Measures the live compiled graph without the golden test's concurrent
    /// reference pass or multi-gigabyte byte-for-byte comparison.
    #[test]
    #[ignore = "runs the full wipneus5.dsl capture; use --release"]
    fn benchmark_compiled_graph_runtime() {
        let capture = repo_path("_captures/wipneus5.dsl");
        assert!(capture.exists(), "capture not found: {}", capture.display());

        let output = tempfile::tempdir().unwrap();
        let widget = golden_widget(&capture, output.path());
        let mut ctx = CompileCtx::default();
        let start = std::time::Instant::now();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx)
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));
        run.wait();
        let elapsed = start.elapsed();
        let files = bin_files(output.path());
        let bytes: u64 = files
            .iter()
            .map(|name| std::fs::metadata(output.path().join(name)).unwrap().len())
            .sum();
        eprintln!(
            "compiled graph: elapsed={:.3}s files={} bytes={bytes}",
            elapsed.as_secs_f64(),
            files.len()
        );
        assert!(!files.is_empty(), "compiled graph produced no output");
    }

    #[test]
    #[ignore = "runs the full wipneus5.dsl capture; use --release"]
    fn benchmark_reference_pipeline_runtime() {
        let capture = repo_path("_captures/wipneus5.dsl");
        assert!(capture.exists(), "capture not found: {}", capture.display());

        let output = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        run_reference(&capture, output.path());
        let elapsed = start.elapsed();
        let files = bin_files(output.path());
        let bytes: u64 = files
            .iter()
            .map(|name| std::fs::metadata(output.path().join(name)).unwrap().len())
            .sum();
        eprintln!(
            "reference pipeline: elapsed={:.3}s files={} bytes={bytes}",
            elapsed.as_secs_f64(),
            files.len()
        );
        assert!(!files.is_empty(), "reference pipeline produced no output");
    }

    #[test]
    #[ignore = "runs the current full pipeline topology; use --release"]
    fn benchmark_current_reference_pipeline_runtime() {
        let capture = repo_path("_captures/wipneus5.dsl");
        let output = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        run_current_reference(&capture, output.path());
        let elapsed = start.elapsed();
        let files = bin_files(output.path());
        let bytes: u64 = files
            .iter()
            .map(|name| std::fs::metadata(output.path().join(name)).unwrap().len())
            .sum();
        eprintln!(
            "current reference: elapsed={:.3}s files={} bytes={bytes}",
            elapsed.as_secs_f64(),
            files.len()
        );
        assert!(!files.is_empty());
    }

    #[test]
    #[ignore = "runs the full SPI-controlled test graph; use --release"]
    fn benchmark_spi_controlled_test_graph_runtime() {
        let capture = repo_path("_captures/wipneus5.dsl");
        let mut widget = startup_widget();
        let output = tempfile::tempdir().unwrap();
        for node in widget.graph_mut().nodes.values_mut() {
            match node.def_name() {
                "DSL File Source" => {
                    node.state = serde_json::to_value(nodes::DslFileSourceState {
                        file: node_graph::FileValue::new(capture.display().to_string()),
                        channels: node_graph::IntValue::new(11, 1, 32),
                    })
                    .unwrap();
                }
                "String Formatter" => {
                    node.state = serde_json::to_value(nodes::StringFormatterState {
                        template: node_graph::StringValue::new(format!(
                            "{}/capture_{{n:04}}.bin",
                            output.path().display()
                        )),
                    })
                    .unwrap();
                }
                _ => {}
            }
        }

        let mut ctx = CompileCtx::default();
        let start = std::time::Instant::now();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx)
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));
        run.wait();
        let elapsed = start.elapsed();
        let files = bin_files(output.path());
        let bytes: u64 = files
            .iter()
            .map(|name| std::fs::metadata(output.path().join(name)).unwrap().len())
            .sum();
        eprintln!(
            "test graph: elapsed={:.3}s files={} bytes={bytes}",
            elapsed.as_secs_f64(),
            files.len()
        );
        assert!(!files.is_empty(), "test graph produced no output");
    }

    #[test]
    #[ignore = "runs the full graph while simulating a 60 Hz 5120-pixel viewer; use --release"]
    fn benchmark_spi_controlled_test_graph_with_live_viewer_queries() {
        let capture = repo_path("_captures/wipneus5.dsl");
        let mut widget = startup_widget();
        let output = tempfile::tempdir().unwrap();
        for node in widget.graph_mut().nodes.values_mut() {
            match node.def_name() {
                "DSL File Source" => {
                    node.state = serde_json::to_value(nodes::DslFileSourceState {
                        file: node_graph::FileValue::new(capture.display().to_string()),
                        channels: node_graph::IntValue::new(11, 1, 32),
                    })
                    .unwrap();
                }
                "String Formatter" => {
                    node.state = serde_json::to_value(nodes::StringFormatterState {
                        template: node_graph::StringValue::new(format!(
                            "{}/capture_{{n:04}}.bin",
                            output.path().display()
                        )),
                    })
                    .unwrap();
                }
                _ => {}
            }
        }

        const TARGET_POINTS: usize = 5_120;
        const END_NS: u64 = 250_000_000_000;
        let mut ctx = CompileCtx::default();
        let start = Instant::now();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx)
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));
        let mut generations = HashMap::new();
        let mut sampled_at = HashMap::new();
        let mut query_time = Duration::ZERO;
        let mut query_count = 0u64;
        while !run.is_finished() {
            let frame_start = Instant::now();
            let queries: Vec<_> = run
                .lanes
                .read()
                .iter()
                .filter_map(|lane| match &lane.data {
                    DerivedLaneData::IndexedAnnotations(indexed) => {
                        Some((lane.name.clone(), Arc::clone(indexed.query())))
                    }
                    _ => None,
                })
                .collect();
            for (name, query) in queries {
                let metadata = query.metadata();
                if generations.get(&name) == Some(&metadata.generation) {
                    continue;
                }
                if metadata.is_live
                    && sampled_at.get(&name).is_some_and(|sampled: &Instant| {
                        sampled.elapsed() < Duration::from_millis(50)
                    })
                {
                    continue;
                }
                sampled_at.insert(name.clone(), Instant::now());
                generations.insert(name, metadata.generation);
                let query_start = Instant::now();
                let buckets = query
                    .coarse_presence_window(0, END_NS, TARGET_POINTS)
                    .unwrap();
                let estimated_words = buckets
                    .iter()
                    .map(|bucket| bucket.word_count)
                    .fold(0u64, u64::saturating_add);
                if estimated_words <= (TARGET_POINTS * 2) as u64 {
                    let _ = query.exact_window(0, END_NS, TARGET_POINTS * 2).unwrap();
                }
                query_time += query_start.elapsed();
                query_count += 1;
            }
            let remaining = Duration::from_millis(16).saturating_sub(frame_start.elapsed());
            std::thread::sleep(remaining);
        }
        run.wait();
        eprintln!(
            "live viewer graph: elapsed={:.3}s queries={query_count} query_time={:.3}s",
            start.elapsed().as_secs_f64(),
            query_time.as_secs_f64()
        );
        assert!(query_count > 0, "viewer lane produced no live queries");
        assert!(
            !bin_files(output.path()).is_empty(),
            "test graph produced no output"
        );
    }

    /// The golden correctness gate: the compiled startup graph must
    /// produce byte-identical output to the hand-built Phase-1 pipeline.
    /// Slow (full 12.7B-sample capture) — run explicitly:
    /// `cargo test -p logic-analyzer-graph --release -- --ignored golden`
    #[test]
    #[ignore = "runs the full wipneus5.dsl capture; use --release"]
    fn golden_compiled_graph_matches_reference() {
        let capture = repo_path("_captures/wipneus5.dsl");
        assert!(capture.exists(), "capture not found: {}", capture.display());

        let tmp = tempfile::tempdir().unwrap();
        let graph_dir = tmp.path().join("graph");
        let ref_dir = tmp.path().join("reference");
        std::fs::create_dir_all(&graph_dir).unwrap();
        std::fs::create_dir_all(&ref_dir).unwrap();

        // The reference pipeline is a second, entirely independent full pass
        // over the same multi-billion-sample capture (own process, own
        // output dir) — nothing about it depends on the compiled-graph run
        // below, so it runs concurrently on its own thread instead of
        // afterward, roughly halving this test's wall-clock time on a
        // machine with room for both.
        let reference_handle = {
            let capture = capture.clone();
            let ref_dir = ref_dir.clone();
            std::thread::spawn(move || run_reference(&capture, &ref_dir))
        };

        // Compiled-graph run: startup graph with capture path + output
        // template pointed at the temp dirs.
        let widget = golden_widget(&capture, &graph_dir);

        // Through the live path: shared sender lists + supervisor-driven
        // shutdown must reproduce the offline byte-exact behavior.
        let mut ctx = CompileCtx::default();
        let lanes = ctx.derived_lanes.clone();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx)
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));
        run.wait();

        // The viewer lanes filled while the pipeline ran.
        {
            let lanes = lanes.read();
            assert_eq!(lanes.len(), 5, "expected 5 viewer lanes");
            let annotations = lanes
                .iter()
                .find_map(|lane| match &lane.data {
                    signal_processing::DerivedLaneData::Annotations(a) => Some(a.len()),
                    signal_processing::DerivedLaneData::IndexedAnnotations(indexed) => {
                        Some(indexed.metadata().total_word_count as usize)
                    }
                    _ => None,
                })
                .expect("a words lane");
            assert!(annotations > 0, "words lane stayed empty");
            let markers: usize = lanes
                .iter()
                .filter_map(|lane| match &lane.data {
                    signal_processing::DerivedLaneData::Markers(m) => Some(m.len()),
                    _ => None,
                })
                .sum();
            // 26 windows → at least 26 start + 26 stop triggers.
            assert!(markers >= 52, "expected ≥52 trigger markers, got {markers}");
        }

        reference_handle.join().expect("reference run panicked");

        let graph_files = bin_files(&graph_dir);
        let ref_files = bin_files(&ref_dir);
        assert!(!ref_files.is_empty(), "reference produced no files");
        assert_eq!(graph_files, ref_files, "different file sets");
        for name in &ref_files {
            let a = std::fs::read(graph_dir.join(name)).unwrap();
            let b = std::fs::read(ref_dir.join(name)).unwrap();
            assert_eq!(a, b, "{name} differs");
        }
        assert_eq!(
            normalized_csv(&graph_dir),
            normalized_csv(&ref_dir),
            "captures.csv differs"
        );
    }
}
