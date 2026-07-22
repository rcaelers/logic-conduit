//! Presentation-neutral collection of typed derived streams into retained storage.

use std::any::Any;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};

use web_time::Instant;

use crate::collected_payload::{CollectedLaneIngestor, CollectedPayloadDescriptor};
use crate::derived_index::{AppendOnlyMipmap, ChunkedMipmap, LaneFold, MipmapRecord};
use crate::derived_word_store::{
    AnnotationQuery, AnnotationStoreBackend, AnnotationStoreMetadata, AnnotationStoreWriterBackend,
    IndexedAnnotationStore, IndexedAnnotationWriter, LiveStoreConfig, LiveStoreMetadata,
    StoreStatus,
};
use crate::errors::{WorkError, WorkResult};
use crate::events::{Annotation, NumberSample, TextSample, Trigger, Word};
use crate::node::ProcessNode;
use crate::ports::{InputPort, OutputPort, PortDirection, PortSchema};
use crate::sample::Sample;

#[derive(Clone, Default)]
pub struct DerivedDataCollectorMetrics {
    inner: Arc<DerivedDataCollectorMetricsInner>,
}

#[derive(Default)]
struct DerivedDataCollectorMetricsInner {
    drain_ns: AtomicU64,
    append_ns: AtomicU64,
    items: AtomicU64,
    batches: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DerivedDataCollectorMetricsSnapshot {
    pub drain_ns: u64,
    pub append_ns: u64,
    pub items: u64,
    pub batches: u64,
}

impl DerivedDataCollectorMetrics {
    pub fn snapshot(&self) -> DerivedDataCollectorMetricsSnapshot {
        DerivedDataCollectorMetricsSnapshot {
            drain_ns: self.inner.drain_ns.load(Ordering::Relaxed),
            append_ns: self.inner.append_ns.load(Ordering::Relaxed),
            items: self.inner.items.load(Ordering::Relaxed),
            batches: self.inner.batches.load(Ordering::Relaxed),
        }
    }

    fn record_drain(&self, started: Instant, items: usize) {
        self.inner.drain_ns.fetch_add(
            started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        if items > 0 {
            self.inner.items.fetch_add(items as u64, Ordering::Relaxed);
            self.inner.batches.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_append(&self, started: Instant) {
        self.inner.append_ns.fetch_add(
            started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
    }
}

/// Suggested per-lane limit for continuous sources that explicitly select
/// rolling in-memory exact-detail retention. Native indexed word lanes do
/// not use this limit because their complete exact history is disk-backed.
pub const DEFAULT_DERIVED_DATA_MAX_ENTRIES: usize = 1_000_000;

/// Most items one lane drains from its channel per `work()` call. Bounds how
/// long one call holds `DerivedLanes`' write lock and, more importantly,
/// stops `DerivedDataCollector` from racing a fast producer to keep its channel
/// perpetually empty — a channel that's allowed to actually fill is what
/// lets its `Block` overflow policy engage and slow the producer down.
const DRAIN_BATCH_SIZE: usize = 65_536;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DerivedDataRetention {
    #[default]
    Unlimited,
    MaxEntries(usize),
}

impl DerivedDataRetention {
    fn trim_target(self, len: usize) -> Option<usize> {
        let Self::MaxEntries(max) = self else {
            return None;
        };
        let max = max.max(1);
        (len > max).then_some((max - max / 4).max(1))
    }
}

#[derive(Debug, Clone)]
pub enum DerivedLaneData {
    /// A boolean level stream, rendered like a channel waveform.
    Digital(Vec<Sample>),
    /// Word boxes. A word carrying a real duration is stored closed with
    /// its true `end_ns`; adjacent instantaneous words meet within a burst,
    /// while a cadence-bounded end leaves long decoding gaps empty.
    Annotations(Vec<Annotation>),
    /// Indexed word lane. Rendering and cursor code query this handle without
    /// retaining every annotation in UI-owned memory.
    IndexedAnnotations(IndexedAnnotationLane),
    /// Zero-width event markers (trigger timestamps, ns).
    Markers(Vec<u64>),
    /// Labeled level values, each valid until the following entry.
    Values(CollectedValueLane),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectedValueKind {
    Number,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedValue {
    pub value: String,
    pub start_time_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedValueLane {
    pub kind: CollectedValueKind,
    pub values: Vec<CollectedValue>,
}

#[derive(Clone)]
pub struct IndexedAnnotationLane {
    query: Arc<dyn AnnotationQuery>,
    store: IndexedAnnotationStore,
}

impl IndexedAnnotationLane {
    pub fn from_store(store: IndexedAnnotationStore) -> Self {
        Self {
            query: Arc::new(store.clone()),
            store,
        }
    }

    pub fn metadata(&self) -> AnnotationStoreMetadata {
        self.query.metadata()
    }

    pub fn query(&self) -> &Arc<dyn AnnotationQuery> {
        &self.query
    }

    pub fn status(&self) -> StoreStatus {
        AnnotationStoreBackend::snapshot(&self.store)
            .metadata
            .status
    }

    pub fn storage_metadata(&self) -> LiveStoreMetadata {
        AnnotationStoreBackend::snapshot(&self.store).metadata
    }

    /// Returns the platform-neutral indexed store handle. Native-only
    /// diagnostics remain methods of the native store implementation rather
    /// than becoming capabilities of a generic viewer lane.
    pub fn store(&self) -> IndexedAnnotationStore {
        self.store.clone()
    }
}

impl std::fmt::Debug for IndexedAnnotationLane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IndexedAnnotationLane")
            .field("metadata", &self.metadata())
            .field("status", &self.status())
            .finish()
    }
}

/// How each lane kind folds into [`MipmapRecord`]s — see
/// `derived_index` for why this exists (a multi-resolution index
/// so the viewer never rescans a whole lane just to render or measure a
/// zoomed-out window).
#[derive(Debug, Clone, Copy)]
pub struct DigitalFold;
impl LaneFold<Sample> for DigitalFold {
    fn leaf(entry: &Sample) -> MipmapRecord {
        MipmapRecord {
            start_ns: entry.start_time_ns,
            end_ns: entry.start_time_ns,
            count: 1,
            level_hint: Some((entry.value, entry.value)),
        }
    }
    fn combine(records: &[MipmapRecord]) -> MipmapRecord {
        let first = records[0];
        let last = records[records.len() - 1];
        MipmapRecord {
            start_ns: first.start_ns,
            end_ns: last.end_ns,
            count: records.iter().map(|record| record.count).sum(),
            level_hint: match (first.level_hint, last.level_hint) {
                (Some((first, _)), Some((_, last))) => Some((first, last)),
                _ => None,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AnnotationFold;
impl LaneFold<Annotation> for AnnotationFold {
    fn leaf(entry: &Annotation) -> MipmapRecord {
        MipmapRecord {
            start_ns: entry.start_ns,
            end_ns: entry.end_ns,
            count: 1,
            level_hint: None,
        }
    }
    fn combine(records: &[MipmapRecord]) -> MipmapRecord {
        MipmapRecord {
            start_ns: records[0].start_ns,
            // Not necessarily the last record in append order — boxes can,
            // in principle, close later than a subsequent one starts.
            end_ns: records.iter().map(|record| record.end_ns).max().unwrap(),
            count: records.iter().map(|record| record.count).sum(),
            level_hint: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MarkerFold;
impl LaneFold<u64> for MarkerFold {
    fn leaf(entry: &u64) -> MipmapRecord {
        MipmapRecord {
            start_ns: *entry,
            end_ns: *entry,
            count: 1,
            level_hint: None,
        }
    }
    fn combine(records: &[MipmapRecord]) -> MipmapRecord {
        MipmapRecord {
            start_ns: records[0].start_ns,
            end_ns: records[records.len() - 1].end_ns,
            count: records.iter().map(|record| record.count).sum(),
            level_hint: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ValueFold;
impl LaneFold<CollectedValue> for ValueFold {
    fn leaf(entry: &CollectedValue) -> MipmapRecord {
        MipmapRecord {
            start_ns: entry.start_time_ns,
            end_ns: entry.start_time_ns,
            count: 1,
            level_hint: None,
        }
    }
    fn combine(records: &[MipmapRecord]) -> MipmapRecord {
        MipmapRecord {
            start_ns: records[0].start_ns,
            end_ns: records[records.len() - 1].end_ns,
            count: records.iter().map(|record| record.count).sum(),
            level_hint: None,
        }
    }
}

/// The multi-resolution index kept alongside an in-memory lane's raw data.
/// Indexed annotations own their presence index behind the query handle, so
/// their summary variant is only a lane-kind marker.
#[derive(Debug, Clone)]
pub enum LaneSummary {
    Digital(AppendOnlyMipmap<Sample, DigitalFold>),
    Annotations(ChunkedMipmap<Annotation, AnnotationFold>),
    IndexedAnnotations,
    Markers(AppendOnlyMipmap<u64, MarkerFold>),
    Values(AppendOnlyMipmap<CollectedValue, ValueFold>),
}

impl LaneSummary {
    /// A summary backfilled from `data` — every production collector registers
    /// a lane with fresh empty storage, so this is normally a no-op, but the invariant "summary
    /// mirrors data" has to hold for *any* caller, not just the ones that
    /// happen to start empty.
    fn matching(data: &DerivedLaneData) -> Self {
        match data {
            DerivedLaneData::Digital(samples) => {
                let mut summary = AppendOnlyMipmap::new();
                summary.extend(samples);
                Self::Digital(summary)
            }
            DerivedLaneData::Annotations(annotations) => {
                let mut summary = ChunkedMipmap::new();
                // Same rule as live appends (`append_word_batch`): an entry
                // with `end_ns == start_ns` is still "open" — not yet
                // closed by a successor — and can't join the summary until
                // it is (the mipmap can never retroactively patch one it
                // already folded in).
                summary.extend(
                    annotations
                        .iter()
                        .filter(|annotation| annotation.end_ns != annotation.start_ns),
                );
                Self::Annotations(summary)
            }
            DerivedLaneData::IndexedAnnotations(_) => Self::IndexedAnnotations,
            DerivedLaneData::Markers(markers) => {
                let mut summary = AppendOnlyMipmap::new();
                summary.extend(markers);
                Self::Markers(summary)
            }
            DerivedLaneData::Values(values) => {
                let mut summary = AppendOnlyMipmap::new();
                summary.extend(&values.values);
                Self::Values(summary)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct DerivedLane {
    pub name: String,
    pub data: DerivedLaneData,
    pub summary: LaneSummary,
    pub word_display_format: Option<String>,
}

/// An adapter-owned retained query handle that generic consumers can discover
/// without knowing its concrete payload type.
#[derive(Clone)]
pub struct OpaqueCollectedLane {
    name: String,
    payload: CollectedPayloadDescriptor,
    query: Arc<dyn Any + Send + Sync>,
}

impl OpaqueCollectedLane {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn payload(&self) -> &CollectedPayloadDescriptor {
        &self.payload
    }

    pub fn query<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        Arc::downcast::<T>(Arc::clone(&self.query)).ok()
    }
}

impl std::fmt::Debug for OpaqueCollectedLane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpaqueCollectedLane")
            .field("name", &self.name)
            .field("payload", &self.payload)
            .finish_non_exhaustive()
    }
}

/// Shared, append-only store of derived lanes. Producers and subscribers hold
/// independent clones, so subscribers may attach after collection has begun or
/// completed. A re-run swaps in a fresh store so stale lanes vanish atomically.
#[derive(Debug, Clone, Default)]
pub struct DerivedLanes {
    inner: Arc<RwLock<Vec<DerivedLane>>>,
    opaque: Arc<RwLock<Vec<OpaqueCollectedLane>>>,
}

impl DerivedLanes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an empty lane and returns its index. Lane order is registration
    /// order (= collector wiring order). Registering an existing name normally
    /// reuses data of the same kind. Indexed annotations are replaced even
    /// by another indexed lane so a restarted collector publishes its new
    /// writer's query handle instead of leaving a stale store visible.
    pub fn register(&self, name: impl Into<String>, data: DerivedLaneData) -> usize {
        let name = name.into();
        let mut lanes = self.inner.write().unwrap();
        if let Some(index) = lanes.iter().position(|lane| lane.name == name) {
            let replace =
                std::mem::discriminant(&lanes[index].data) != std::mem::discriminant(&data);
            let replace = replace || matches!(data, DerivedLaneData::IndexedAnnotations(_));
            let replace = replace
                || matches!(
                    (&lanes[index].data, &data),
                    (DerivedLaneData::Values(current), DerivedLaneData::Values(next))
                        if current.kind != next.kind
                );
            if replace {
                lanes[index].summary = LaneSummary::matching(&data);
                lanes[index].data = data;
            }
            return index;
        }
        let summary = LaneSummary::matching(&data);
        lanes.push(DerivedLane {
            name,
            data,
            summary,
            word_display_format: None,
        });
        lanes.len() - 1
    }

    /// Read access for rendering.
    pub fn read(&self) -> RwLockReadGuard<'_, Vec<DerivedLane>> {
        self.inner.read().unwrap()
    }

    /// Publishes an adapter-owned retained query. A later subscriber can
    /// attach after collection has completed and downcast only to the payload
    /// type it registered.
    pub fn publish_opaque_lane<T: Send + Sync + 'static>(
        &self,
        name: impl Into<String>,
        payload: CollectedPayloadDescriptor,
        query: Arc<T>,
    ) {
        let name = name.into();
        let lane = OpaqueCollectedLane {
            name: name.clone(),
            payload,
            query,
        };
        let mut lanes = self.opaque.write().unwrap();
        if let Some(index) = lanes.iter().position(|existing| existing.name == name) {
            lanes[index] = lane;
        } else {
            lanes.push(lane);
        }
    }

    pub fn opaque_lanes(&self) -> Vec<OpaqueCollectedLane> {
        self.opaque.read().unwrap().clone()
    }

    fn set_word_display_format(&self, index: usize, format: Option<String>) {
        if let Some(lane) = self.inner.write().unwrap().get_mut(index) {
            lane.word_display_format = format;
        }
    }

    /// Appends a whole batch under a single write-lock acquisition — called
    /// once per `DerivedDataCollector::work()` invocation per lane, rather than once
    /// per item, so a burst of decoded entries doesn't take (and contend
    /// the UI thread's `read()` for) the lock once per item.
    fn append_digital_batch_retained(
        &self,
        lane: usize,
        samples: impl IntoIterator<Item = Sample>,
        retention: DerivedDataRetention,
    ) {
        let mut lanes = self.inner.write().unwrap();
        let Some(lane) = lanes.get_mut(lane) else {
            return;
        };
        let (DerivedLaneData::Digital(existing), LaneSummary::Digital(summary)) =
            (&mut lane.data, &mut lane.summary)
        else {
            return;
        };
        for sample in samples {
            summary.push(&sample);
            existing.push(sample);
        }
        if let Some(target) = retention.trim_target(existing.len()) {
            existing.drain(..existing.len() - target);
        }
    }

    /// Items are `(start_ns, duration_ns, value)` — [`Word`]'s shape.
    fn append_word_batch_retained(
        &self,
        lane: usize,
        words: impl IntoIterator<Item = (u64, u64, u64)>,
        retention: DerivedDataRetention,
    ) {
        let mut lanes = self.inner.write().unwrap();
        let Some(lane) = lanes.get_mut(lane) else {
            return;
        };
        let (DerivedLaneData::Annotations(annotations), LaneSummary::Annotations(summary)) =
            (&mut lane.data, &mut lane.summary)
        else {
            return;
        };
        for (start_ns, duration_ns, value) in words {
            let previous_start_ns = annotations
                .len()
                .checked_sub(2)
                .map(|index| annotations[index].start_ns);
            if let Some(previous) = annotations.last_mut()
                && previous.end_ns == previous.start_ns
            {
                previous.end_ns = crate::events::instantaneous_word_end_ns(
                    previous_start_ns,
                    previous.start_ns,
                    start_ns,
                );
                // Only now that its `end_ns` is final can it join the
                // summary — the mipmap is append-only and can never
                // retroactively patch an entry once it's folded into a
                // coarser tier, so the most recent annotation always lags
                // the summary by exactly one entry until the next word
                // closes or cadence-bounds it (or, if the run ends right after it, forever —
                // the raw `data` entry is still fully correct and is what
                // exact/near-zoom rendering reads directly; see
                // `draw_derived_annotations`'s open-ended handling).
                summary.push(previous);
            }
            let annotation = Annotation {
                start_ns,
                // A word with a real duration is closed right away at its
                // true end; an instantaneous one stays "open" (end ==
                // start) until the next word patches or cadence-bounds it.
                end_ns: start_ns + duration_ns,
                value,
            };
            if duration_ns > 0 {
                summary.push(&annotation);
            }
            annotations.push(annotation);
        }
        if let Some(target) = retention.trim_target(annotations.len()) {
            annotations.drain(..annotations.len() - target);
        }
    }

    fn append_marker_batch_retained(
        &self,
        lane: usize,
        timestamps: impl IntoIterator<Item = u64>,
        retention: DerivedDataRetention,
    ) {
        let mut lanes = self.inner.write().unwrap();
        let Some(lane) = lanes.get_mut(lane) else {
            return;
        };
        let (DerivedLaneData::Markers(markers), LaneSummary::Markers(summary)) =
            (&mut lane.data, &mut lane.summary)
        else {
            return;
        };
        for timestamp_ns in timestamps {
            summary.push(&timestamp_ns);
            markers.push(timestamp_ns);
        }
        if let Some(target) = retention.trim_target(markers.len()) {
            markers.drain(..markers.len() - target);
        }
    }

    fn append_value_batch_retained(
        &self,
        lane: usize,
        values: impl IntoIterator<Item = CollectedValue>,
        retention: DerivedDataRetention,
    ) {
        let mut lanes = self.inner.write().unwrap();
        let Some(lane) = lanes.get_mut(lane) else {
            return;
        };
        let (DerivedLaneData::Values(existing), LaneSummary::Values(summary)) =
            (&mut lane.data, &mut lane.summary)
        else {
            return;
        };
        for value in values {
            summary.push(&value);
            existing.values.push(value);
        }
        if let Some(target) = retention.trim_target(existing.values.len()) {
            existing.values.drain(..existing.values.len() - target);
        }
    }
}

/// The generic shapes retained derived data can take. Every producer output
/// reduces to one of these without naming any eventual presentation subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectedDataKind {
    Signal,
    Words,
    Trigger,
    Number,
    Text,
}

enum LaneBuffer {
    Signal(VecDeque<Sample>),
    Words(VecDeque<Word>),
    Trigger(VecDeque<Trigger>),
    Number(VecDeque<NumberSample>),
    Text(VecDeque<TextSample>),
}

struct Lane {
    kind: CollectedDataKind,
    store_index: usize,
    buffer: LaneBuffer,
    eos: bool,
    word_writer: Option<IndexedAnnotationWriter>,
    word_indexed: bool,
}

enum CollectorLane {
    BuiltIn(Box<Lane>),
    Adapter(Box<dyn CollectedLaneIngestor>),
}

impl CollectorLane {
    fn is_finished(&self) -> bool {
        match self {
            Self::BuiltIn(lane) => lane.eos,
            Self::Adapter(lane) => lane.is_finished(),
        }
    }
}

/// Sink with one typed input per lane. Never blocks *waiting* on any single
/// input — lanes drain round-robin with `try_recv` so a quiet lane cannot
/// stall a busy one — but each lane's channel is drained in bounded batches
/// (`DRAIN_BATCH_SIZE`), not to exhaustion, so a channel that a producer is
/// filling faster than this sink drains it stays full and the producer's
/// own send genuinely blocks (`docs/PIPELINE_DESIGN.md`, flow control) — real
/// backpressure, not a silent drop once storage fills up.
pub struct DerivedDataCollector {
    name: String,
    store: DerivedLanes,
    lanes: Vec<CollectorLane>,
    retention: DerivedDataRetention,
    metrics: Option<DerivedDataCollectorMetrics>,
    indexed_words: bool,
    word_store_config: LiveStoreConfig,
}

impl DerivedDataCollector {
    pub fn new(store: DerivedLanes) -> Self {
        Self {
            name: "derived-data-collector".to_string(),
            store,
            lanes: Vec::new(),
            retention: DerivedDataRetention::Unlimited,
            metrics: None,
            indexed_words: true,
            word_store_config: LiveStoreConfig::default(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_retention(mut self, retention: DerivedDataRetention) -> Self {
        self.retention = retention;
        self
    }

    pub fn with_metrics(mut self, metrics: DerivedDataCollectorMetrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Selects indexed storage for subsequently added word lanes.
    pub fn with_indexed_words(mut self, enabled: bool) -> Self {
        self.indexed_words = enabled;
        self
    }

    /// Overrides the indexed-store configuration for subsequently added word
    /// lanes. Platform backends interpret the capabilities they support.
    pub fn with_word_store_config(mut self, config: LiveStoreConfig) -> Self {
        self.word_store_config = config;
        self
    }

    /// Appends a lane; input port order follows lane order (`in0`, `in1`, …).
    pub fn with_lane(mut self, kind: CollectedDataKind, name: impl Into<String>) -> Self {
        let name = name.into();
        let (data, word_writer, word_indexed) = match kind {
            CollectedDataKind::Signal => (DerivedLaneData::Digital(Vec::new()), None, false),
            CollectedDataKind::Words if self.indexed_words => {
                if let Some(persistent) = self.word_store_config.persistence.as_ref() {
                    match IndexedAnnotationStore::open_persistent(persistent) {
                        Ok(Some(store)) => {
                            let data = DerivedLaneData::IndexedAnnotations(
                                IndexedAnnotationLane::from_store(store),
                            );
                            let store_index = self.store.register(name, data);
                            self.lanes.push(CollectorLane::BuiltIn(Box::new(Lane {
                                kind,
                                store_index,
                                buffer: LaneBuffer::Words(VecDeque::new()),
                                eos: false,
                                word_writer: None,
                                word_indexed: true,
                            })));
                            return self;
                        }
                        Ok(None) => {}
                        Err(error) => tracing::warn!(
                            lane = %name,
                            %error,
                            "invalid persistent derived-data cache; rebuilding"
                        ),
                    }
                }
                match IndexedAnnotationWriter::create(self.word_store_config.clone()) {
                    Ok((writer, store)) => (
                        DerivedLaneData::IndexedAnnotations(IndexedAnnotationLane::from_store(
                            store,
                        )),
                        Some(writer),
                        true,
                    ),
                    Err(error) => {
                        tracing::warn!(
                            lane = %name,
                            %error,
                            "could not create indexed derived-data word lane; using in-memory storage"
                        );
                        (DerivedLaneData::Annotations(Vec::new()), None, false)
                    }
                }
            }
            CollectedDataKind::Words => (DerivedLaneData::Annotations(Vec::new()), None, false),
            CollectedDataKind::Trigger => (DerivedLaneData::Markers(Vec::new()), None, false),
            CollectedDataKind::Number | CollectedDataKind::Text => {
                let kind = if kind == CollectedDataKind::Number {
                    CollectedValueKind::Number
                } else {
                    CollectedValueKind::Text
                };
                (
                    DerivedLaneData::Values(CollectedValueLane {
                        kind,
                        values: Vec::new(),
                    }),
                    None,
                    false,
                )
            }
        };
        let buffer = match kind {
            CollectedDataKind::Signal => LaneBuffer::Signal(VecDeque::new()),
            CollectedDataKind::Words => LaneBuffer::Words(VecDeque::new()),
            CollectedDataKind::Trigger => LaneBuffer::Trigger(VecDeque::new()),
            CollectedDataKind::Number => LaneBuffer::Number(VecDeque::new()),
            CollectedDataKind::Text => LaneBuffer::Text(VecDeque::new()),
        };
        let store_index = self.store.register(name, data);
        self.lanes.push(CollectorLane::BuiltIn(Box::new(Lane {
            kind,
            store_index,
            buffer,
            eos: false,
            word_writer,
            word_indexed,
        })));
        self
    }

    /// Adds a lane implemented by a registered payload adapter. The adapter
    /// owns typed draining and retained storage; this collector only
    /// schedules it alongside the built-in lanes.
    pub fn with_ingestor(mut self, ingestor: Box<dyn CollectedLaneIngestor>) -> Self {
        self.lanes.push(CollectorLane::Adapter(ingestor));
        self
    }

    pub fn with_lane_format(
        self,
        kind: CollectedDataKind,
        name: impl Into<String>,
        format: Option<String>,
    ) -> Self {
        let sink = self.with_lane(kind, name);
        if let Some(CollectorLane::BuiltIn(last)) = sink.lanes.last() {
            sink.store.set_word_display_format(last.store_index, format);
        }
        sink
    }
}

impl ProcessNode for DerivedDataCollector {
    fn name(&self) -> &str {
        &self.name
    }

    fn should_stop(&self) -> bool {
        !self.lanes.is_empty() && self.lanes.iter().all(CollectorLane::is_finished)
    }

    fn input_scheduling(&self) -> crate::node::InputScheduling {
        crate::node::InputScheduling::Any
    }

    fn num_inputs(&self) -> usize {
        self.lanes.len()
    }

    fn num_outputs(&self) -> usize {
        0
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        self.lanes
            .iter()
            .enumerate()
            .map(|(index, lane)| {
                let name = format!("in{index}");
                match lane {
                    CollectorLane::BuiltIn(lane) => match lane.kind {
                        CollectedDataKind::Signal => {
                            PortSchema::new::<Sample>(name, index, PortDirection::Input)
                        }
                        CollectedDataKind::Words => {
                            PortSchema::new::<Word>(name, index, PortDirection::Input)
                        }
                        CollectedDataKind::Trigger => {
                            PortSchema::new::<Trigger>(name, index, PortDirection::Input)
                        }
                        CollectedDataKind::Number => {
                            PortSchema::new::<NumberSample>(name, index, PortDirection::Input)
                        }
                        CollectedDataKind::Text => {
                            PortSchema::new::<TextSample>(name, index, PortDirection::Input)
                        }
                    },
                    CollectorLane::Adapter(lane) => lane.input_schema(index),
                }
            })
            .collect()
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        use crossbeam_channel::TryRecvError;

        let store = self.store.clone();
        let metrics = self.metrics.clone();
        let mut progress = 0usize;

        for (index, lane) in self.lanes.iter_mut().enumerate() {
            if lane.is_finished() {
                continue;
            }
            let port = inputs
                .get(index)
                .ok_or_else(|| WorkError::NodeError(format!("missing collector input {index}")))?;

            let CollectorLane::BuiltIn(lane) = lane else {
                let CollectorLane::Adapter(lane) = lane else {
                    unreachable!("collector lane variant is exhaustive")
                };
                let started = metrics.as_ref().map(|_| Instant::now());
                let lane_progress = lane.drain(port, self.retention)?;
                progress += lane_progress;
                if let (Some(metrics), Some(started)) = (&metrics, started) {
                    metrics.record_drain(started, lane_progress);
                    if lane_progress > 0 {
                        metrics.record_append(started);
                    }
                }
                continue;
            };

            // Bounded, not drained to exhaustion: letting a burst fill this
            // lane's channel (rather than racing to empty it every call) is
            // what makes the channel's own bound + `Block` overflow policy
            // (`docs/PIPELINE_DESIGN.md`, flow control) actually apply real
            // backpressure to the producer instead of never engaging.
            macro_rules! drain_batch {
                ($ty:ty, $buffer:expr) => {{
                    let drain_started = metrics.as_ref().map(|_| Instant::now());
                    let mut batch: Vec<$ty> = Vec::with_capacity(DRAIN_BATCH_SIZE);
                    if let Some(mut receiver) = port.get::<$ty>($buffer) {
                        match receiver.try_recv_many(&mut batch, DRAIN_BATCH_SIZE) {
                            Ok(_) | Err(TryRecvError::Empty) => {}
                            Err(TryRecvError::Disconnected) => lane.eos = true,
                        }
                    } else {
                        // Unconnected input: nothing will ever arrive.
                        lane.eos = true;
                    }
                    if let (Some(metrics), Some(started)) = (&metrics, drain_started) {
                        metrics.record_drain(started, batch.len());
                    }
                    batch
                }};
            }

            match &mut lane.buffer {
                LaneBuffer::Signal(buffer) => {
                    let batch = drain_batch!(Sample, buffer);
                    progress += batch.len();
                    if !batch.is_empty() {
                        let append_started = metrics.as_ref().map(|_| Instant::now());
                        store.append_digital_batch_retained(
                            lane.store_index,
                            batch,
                            self.retention,
                        );
                        if let (Some(metrics), Some(started)) = (&metrics, append_started) {
                            metrics.record_append(started);
                        }
                    }
                }
                LaneBuffer::Words(buffer) => {
                    let batch = drain_batch!(Word, buffer);
                    progress += batch.len();
                    if !batch.is_empty() {
                        let append_started = metrics.as_ref().map(|_| Instant::now());
                        let indexed = lane.word_indexed;
                        if let Some(writer) = lane.word_writer.as_mut()
                            && let Err(error) =
                                AnnotationStoreWriterBackend::append_batch(writer, &batch)
                        {
                            tracing::warn!(
                                lane = lane.store_index,
                                %error,
                                "indexed derived-data word lane failed; disabling further appends"
                            );
                            lane.word_writer = None;
                        }
                        if !indexed {
                            store.append_word_batch_retained(
                                lane.store_index,
                                batch
                                    .into_iter()
                                    .map(|w| (w.timestamp_ns, w.duration_ns, w.value)),
                                self.retention,
                            );
                        }
                        if let (Some(metrics), Some(started)) = (&metrics, append_started) {
                            metrics.record_append(started);
                        }
                    }
                }
                LaneBuffer::Trigger(buffer) => {
                    let batch = drain_batch!(Trigger, buffer);
                    progress += batch.len();
                    if !batch.is_empty() {
                        let append_started = metrics.as_ref().map(|_| Instant::now());
                        store.append_marker_batch_retained(
                            lane.store_index,
                            batch.into_iter().map(|item| item.timestamp_ns),
                            self.retention,
                        );
                        if let (Some(metrics), Some(started)) = (&metrics, append_started) {
                            metrics.record_append(started);
                        }
                    }
                }
                LaneBuffer::Number(buffer) => {
                    let batch = drain_batch!(NumberSample, buffer);
                    progress += batch.len();
                    if !batch.is_empty() {
                        let append_started = metrics.as_ref().map(|_| Instant::now());
                        store.append_value_batch_retained(
                            lane.store_index,
                            batch.into_iter().map(|sample| CollectedValue {
                                value: sample.value.to_string(),
                                start_time_ns: sample.start_time_ns,
                            }),
                            self.retention,
                        );
                        if let (Some(metrics), Some(started)) = (&metrics, append_started) {
                            metrics.record_append(started);
                        }
                    }
                }
                LaneBuffer::Text(buffer) => {
                    let batch = drain_batch!(TextSample, buffer);
                    progress += batch.len();
                    if !batch.is_empty() {
                        let append_started = metrics.as_ref().map(|_| Instant::now());
                        store.append_value_batch_retained(
                            lane.store_index,
                            batch.into_iter().map(|sample| CollectedValue {
                                value: sample.value,
                                start_time_ns: sample.start_time_ns,
                            }),
                            self.retention,
                        );
                        if let (Some(metrics), Some(started)) = (&metrics, append_started) {
                            metrics.record_append(started);
                        }
                    }
                }
            }

            if lane.eos
                && let Some(mut writer) = lane.word_writer.take()
                && let Err(error) = AnnotationStoreWriterBackend::finish(&mut writer)
            {
                tracing::warn!(
                    lane = lane.store_index,
                    %error,
                    "could not finish indexed derived-data word lane"
                );
            }
        }

        if progress == 0 {
            if self.lanes.iter().all(CollectorLane::is_finished) {
                return Err(WorkError::Shutdown);
            }
            // Native thread-driven execution backs off here; cooperative wasm
            // scheduling already yields between work calls.
            crate::idle_backoff();
        }
        Ok(progress)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use crossbeam_channel::bounded;

    use super::*;

    #[derive(Clone)]
    struct PluginEvent(u64);

    struct PluginEventIngestor {
        values: Arc<std::sync::Mutex<Vec<u64>>>,
        buffer: VecDeque<PluginEvent>,
        finished: bool,
    }

    impl CollectedLaneIngestor for PluginEventIngestor {
        fn input_schema(&self, index: usize) -> PortSchema {
            PortSchema::new::<PluginEvent>(format!("in{index}"), index, PortDirection::Input)
        }

        fn drain(
            &mut self,
            input: &InputPort,
            _retention: DerivedDataRetention,
        ) -> WorkResult<usize> {
            use crossbeam_channel::TryRecvError;

            let mut batch = Vec::new();
            if let Some(mut receiver) = input.get(&mut self.buffer) {
                match receiver.try_recv_many(&mut batch, DRAIN_BATCH_SIZE) {
                    Ok(_) | Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => self.finished = true,
                }
            } else {
                self.finished = true;
            }
            let count = batch.len();
            self.values
                .lock()
                .unwrap()
                .extend(batch.into_iter().map(|event| event.0));
            Ok(count)
        }

        fn is_finished(&self) -> bool {
            self.finished
        }
    }
    use crate::ports::OutputPort as OutPort;
    use crate::sender::ChannelMessage;
    use crate::watchdog::Watchdog;

    trait TestAppendBatches {
        fn append_digital_batch<I>(&self, lane: usize, samples: I)
        where
            I: IntoIterator<Item = Sample>;
        fn append_word_batch<I>(&self, lane: usize, words: I)
        where
            I: IntoIterator<Item = (u64, u64, u64)>;
        fn append_marker_batch<I>(&self, lane: usize, timestamps: I)
        where
            I: IntoIterator<Item = u64>;
    }

    impl TestAppendBatches for DerivedLanes {
        fn append_digital_batch<I>(&self, lane: usize, samples: I)
        where
            I: IntoIterator<Item = Sample>,
        {
            self.append_digital_batch_retained(lane, samples, DerivedDataRetention::Unlimited);
        }

        fn append_word_batch<I>(&self, lane: usize, words: I)
        where
            I: IntoIterator<Item = (u64, u64, u64)>,
        {
            self.append_word_batch_retained(lane, words, DerivedDataRetention::Unlimited);
        }

        fn append_marker_batch<I>(&self, lane: usize, timestamps: I)
        where
            I: IntoIterator<Item = u64>,
        {
            self.append_marker_batch_retained(lane, timestamps, DerivedDataRetention::Unlimited);
        }
    }

    fn run_sink(sink: &mut DerivedDataCollector, inputs: Vec<InputPort>) {
        let outputs: Vec<OutPort> = vec![];
        loop {
            match sink.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
    }

    #[test]
    fn adapter_lane_collects_and_publishes_an_opaque_query() {
        let lanes = DerivedLanes::new();
        let mut payloads = crate::CollectedPayloadRegistry::new();
        payloads
            .register::<PluginEvent>("org.example.plugin-event/v1")
            .unwrap();
        let values = Arc::new(std::sync::Mutex::new(Vec::new()));
        lanes.publish_opaque_lane(
            "plugin.events",
            payloads.descriptor::<PluginEvent>().unwrap().clone(),
            Arc::clone(&values),
        );
        let mut collector =
            DerivedDataCollector::new(lanes.clone()).with_ingestor(Box::new(PluginEventIngestor {
                values: Arc::clone(&values),
                buffer: VecDeque::new(),
                finished: false,
            }));

        let watchdog = Watchdog::new();
        let (sender, receiver) = bounded::<ChannelMessage<PluginEvent>>(4);
        sender
            .send(ChannelMessage::Batch(vec![PluginEvent(2), PluginEvent(5)]))
            .unwrap();
        drop(sender);
        run_sink(
            &mut collector,
            vec![InputPort::new_with_watchdog(
                receiver,
                &watchdog,
                "collector",
                "in0",
            )],
        );

        assert_eq!(*values.lock().unwrap(), vec![2, 5]);
        let opaque = lanes.opaque_lanes();
        assert_eq!(opaque[0].name(), "plugin.events");
        assert_eq!(
            opaque[0].payload().stable_id(),
            "org.example.plugin-event/v1"
        );
        assert_eq!(
            *opaque[0]
                .query::<std::sync::Mutex<Vec<u64>>>()
                .unwrap()
                .lock()
                .unwrap(),
            vec![2, 5]
        );
    }

    #[test]
    fn lanes_collect_signals_words_and_triggers() {
        let store = DerivedLanes::new();
        let mut sink = DerivedDataCollector::new(store.clone())
            .with_lane(CollectedDataKind::Signal, "latch.q")
            .with_lane(CollectedDataKind::Words, "decoder.words")
            .with_lane(CollectedDataKind::Trigger, "start.match");

        let wd = Watchdog::new();
        let (sig_tx, sig_rx) = bounded::<ChannelMessage<Sample>>(16);
        sig_tx
            .send(ChannelMessage::Sample(Sample::new(true, 100)))
            .unwrap();
        sig_tx
            .send(ChannelMessage::Sample(Sample::new(false, 300)))
            .unwrap();
        drop(sig_tx);

        let (word_tx, word_rx) = bounded::<ChannelMessage<Word>>(16);
        for (value, ts) in [(0xAB_u64, 1_000_u64), (0xCD, 1_500)] {
            word_tx
                .send(ChannelMessage::Sample(Word::new(value, ts)))
                .unwrap();
        }
        drop(word_tx);

        let (trig_tx, trig_rx) = bounded::<ChannelMessage<Trigger>>(16);
        trig_tx
            .send(ChannelMessage::Sample(Trigger { timestamp_ns: 42 }))
            .unwrap();
        drop(trig_tx);

        let inputs = vec![
            InputPort::new_with_watchdog(sig_rx, &wd, "viewer", "in0"),
            InputPort::new_with_watchdog(word_rx, &wd, "viewer", "in1"),
            InputPort::new_with_watchdog(trig_rx, &wd, "viewer", "in2"),
        ];
        run_sink(&mut sink, inputs);

        let lanes = store.read();
        assert_eq!(lanes.len(), 3);
        assert_eq!(lanes[0].name, "latch.q");
        match &lanes[0].data {
            DerivedLaneData::Digital(samples) => {
                assert_eq!(
                    samples.as_slice(),
                    &[Sample::new(true, 100), Sample::new(false, 300)]
                );
            }
            other => panic!("expected digital lane, got {other:?}"),
        }
        let expected = [
            Annotation {
                start_ns: 1_000,
                end_ns: 1_500,
                value: 0xAB,
            },
            Annotation {
                start_ns: 1_500,
                end_ns: 1_500,
                value: 0xCD,
            },
        ];
        match &lanes[1].data {
            DerivedLaneData::IndexedAnnotations(indexed) => {
                assert_eq!(indexed.status(), StoreStatus::Finished);
                assert_eq!(indexed.metadata().total_word_count, 2);
                assert_eq!(
                    indexed
                        .query
                        .exact_window(0, 2_000, 10)
                        .unwrap()
                        .annotations,
                    expected
                );
            }
            other => panic!("expected indexed annotation lane, got {other:?}"),
        }
        match &lanes[2].data {
            DerivedLaneData::Markers(markers) => assert_eq!(markers.as_slice(), &[42]),
            other => panic!("expected marker lane, got {other:?}"),
        }
    }

    #[test]
    fn lanes_collect_number_and_text_levels() {
        let store = DerivedLanes::new();
        let mut sink = DerivedDataCollector::new(store.clone())
            .with_lane(CollectedDataKind::Number, "counter.count")
            .with_lane(CollectedDataKind::Text, "formatter.text");

        let wd = Watchdog::new();
        let (number_tx, number_rx) = bounded::<ChannelMessage<NumberSample>>(16);
        number_tx
            .send(ChannelMessage::Sample(NumberSample::new(-2, 0)))
            .unwrap();
        number_tx
            .send(ChannelMessage::Sample(NumberSample::new(3, 500)))
            .unwrap();
        drop(number_tx);

        let (text_tx, text_rx) = bounded::<ChannelMessage<TextSample>>(16);
        text_tx
            .send(ChannelMessage::Sample(TextSample::new("Window 03", 500)))
            .unwrap();
        drop(text_tx);

        run_sink(
            &mut sink,
            vec![
                InputPort::new_with_watchdog(number_rx, &wd, "viewer", "in0"),
                InputPort::new_with_watchdog(text_rx, &wd, "viewer", "in1"),
            ],
        );

        let lanes = store.read();
        let DerivedLaneData::Values(numbers) = &lanes[0].data else {
            panic!("expected number values");
        };
        assert_eq!(numbers.kind, CollectedValueKind::Number);
        assert_eq!(
            numbers.values,
            [
                CollectedValue {
                    value: "-2".to_owned(),
                    start_time_ns: 0,
                },
                CollectedValue {
                    value: "3".to_owned(),
                    start_time_ns: 500,
                },
            ]
        );
        let DerivedLaneData::Values(text) = &lanes[1].data else {
            panic!("expected text values");
        };
        assert_eq!(text.kind, CollectedValueKind::Text);
        assert_eq!(
            text.values,
            [CollectedValue {
                value: "Window 03".to_owned(),
                start_time_ns: 500,
            }]
        );
    }

    #[test]
    fn work_drains_at_most_one_batch_per_call() {
        // A single `work()` call must not race a fast producer to keep the
        // channel empty — that's what lets the channel's own bound and
        // `Block` overflow policy apply real backpressure instead of never
        // engaging (§`DRAIN_BATCH_SIZE`).
        let store = DerivedLanes::new();
        let mut sink =
            DerivedDataCollector::new(store.clone()).with_lane(CollectedDataKind::Signal, "sig");

        let total = DRAIN_BATCH_SIZE + 5;
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<Sample>>(total + 1);
        for i in 0..total as u64 {
            tx.send(ChannelMessage::Sample(Sample::new(i % 2 == 0, i)))
                .unwrap();
        }
        drop(tx);
        let inputs = vec![InputPort::new_with_watchdog(rx, &wd, "viewer", "in0")];

        let progress = sink.work(&inputs, &[]).unwrap();
        assert_eq!(progress, DRAIN_BATCH_SIZE, "one call drains one batch");
        {
            let lanes = store.read();
            let DerivedLaneData::Digital(samples) = &lanes[0].data else {
                panic!("expected digital lane");
            };
            assert_eq!(samples.len(), DRAIN_BATCH_SIZE);
        }

        // The remainder (plus the shutdown sentinel) arrives over the
        // following calls.
        run_sink(&mut sink, inputs);
        let lanes = store.read();
        let DerivedLaneData::Digital(samples) = &lanes[0].data else {
            panic!("expected digital lane");
        };
        assert_eq!(samples.len(), total);
    }

    #[test]
    fn instantaneous_annotation_leaves_long_inter_word_gaps_empty() {
        let store = DerivedLanes::new();
        let lane = store.register("w", DerivedLaneData::Annotations(Vec::new()));
        store.append_word_batch(
            lane,
            [
                (1_000, 0, 1),
                (1_100, 0, 2),
                (1_100 + crate::events::MAX_ANNOTATION_NS * 10, 0, 3),
            ],
        );
        let lanes = store.read();
        let DerivedLaneData::Annotations(annotations) = &lanes[0].data else {
            panic!("expected annotations");
        };
        assert_eq!(annotations[0].end_ns, 1_100);
        assert_eq!(annotations[1].end_ns, 1_200);
        assert!(annotations[1].end_ns < annotations[2].start_ns);
    }

    #[test]
    fn instantaneous_annotations_follow_a_slow_burst_cadence() {
        const WORD_PERIOD_NS: u64 = 24_000_000;
        let store = DerivedLanes::new();
        let lane = store.register("w", DerivedLaneData::Annotations(Vec::new()));
        store.append_word_batch(
            lane,
            [
                (1_000_000_000, 0, 1),
                (1_000_000_000 + WORD_PERIOD_NS, 0, 2),
                (1_000_000_000 + WORD_PERIOD_NS * 2, 0, 3),
                (6_000_000_000, 0, 4),
            ],
        );

        let lanes = store.read();
        let DerivedLaneData::Annotations(annotations) = &lanes[0].data else {
            panic!("expected annotations");
        };
        assert_eq!(annotations[0].end_ns, annotations[1].start_ns);
        assert_eq!(annotations[1].end_ns, annotations[2].start_ns);
        assert_eq!(
            annotations[2].end_ns,
            annotations[2].start_ns + WORD_PERIOD_NS
        );
        assert!(annotations[2].end_ns < annotations[3].start_ns);
    }

    #[test]
    fn summary_tracks_digital_samples_as_they_arrive() {
        let store = DerivedLanes::new();
        let lane = store.register("d", DerivedLaneData::Digital(Vec::new()));
        store.append_digital_batch(lane, [Sample::new(true, 100), Sample::new(false, 300)]);
        let lanes = store.read();
        let LaneSummary::Digital(summary) = &lanes[0].summary else {
            panic!("expected a digital summary");
        };
        assert_eq!(summary.len(), 2);
        let window = summary.sampled_window(0, 300, 10);
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].level_hint, Some((true, true)));
        assert_eq!(window[1].level_hint, Some((false, false)));
    }

    #[test]
    fn summary_tracks_markers_as_they_arrive() {
        let store = DerivedLanes::new();
        let lane = store.register("m", DerivedLaneData::Markers(Vec::new()));
        store.append_marker_batch(lane, [10, 20, 30]);
        let lanes = store.read();
        let LaneSummary::Markers(summary) = &lanes[0].summary else {
            panic!("expected a markers summary");
        };
        assert_eq!(summary.len(), 3);
    }

    /// A word carrying a real duration is stored closed at its
    /// true end immediately — never patched to the next word's start, never
    /// left open for the renderer to estimate.
    #[test]
    fn word_with_duration_is_closed_at_its_true_end() {
        let store = DerivedLanes::new();
        let lane = store.register("w", DerivedLaneData::Annotations(Vec::new()));

        // A word spanning 2_300ns, followed much later by another; the first's
        // end must stay its own, not stretch to the second's start.
        store.append_word_batch(lane, [(1_000, 2_300, 0x600081)]);
        {
            let lanes = store.read();
            let DerivedLaneData::Annotations(annotations) = &lanes[0].data else {
                panic!("expected annotations");
            };
            assert_eq!(
                annotations.as_slice(),
                &[Annotation {
                    start_ns: 1_000,
                    end_ns: 3_300,
                    value: 0x600081
                }]
            );
            // Closed immediately → in the summary at once, no one-entry lag.
            let LaneSummary::Annotations(summary) = &lanes[0].summary else {
                panic!("expected an annotations summary");
            };
            assert_eq!(summary.len(), 1);
        }

        store.append_word_batch(lane, [(500_000, 2_300, 0x600000)]);
        let lanes = store.read();
        let DerivedLaneData::Annotations(annotations) = &lanes[0].data else {
            panic!("expected annotations");
        };
        assert_eq!(annotations[0].end_ns, 3_300, "true end must not be patched");
        assert_eq!(annotations[1].end_ns, 502_300);
    }

    #[test]
    fn summary_lags_the_most_recent_open_annotation_by_one() {
        // The mipmap can't retroactively patch an entry once it's pushed,
        // so the most recent (still "open", not yet end-patched) annotation
        // only joins the summary once the *next* word closes it.
        let store = DerivedLanes::new();
        let lane = store.register("w", DerivedLaneData::Annotations(Vec::new()));

        store.append_word_batch(lane, [(1_000, 0, 0xAB)]);
        {
            let lanes = store.read();
            let LaneSummary::Annotations(summary) = &lanes[0].summary else {
                panic!("expected an annotations summary");
            };
            assert_eq!(summary.len(), 0, "the only word so far is still open");
        }

        store.append_word_batch(lane, [(1_500, 0, 0xCD)]);
        {
            let lanes = store.read();
            let LaneSummary::Annotations(summary) = &lanes[0].summary else {
                panic!("expected an annotations summary");
            };
            assert_eq!(summary.len(), 1, "the first word is now closed");
            let window = summary.sampled_window(0, 1_500, 10);
            assert_eq!(window[0].start_ns, 1_000);
            assert_eq!(window[0].end_ns, 1_500);
        }
    }

    #[test]
    fn annotation_chunk_rollover_preserves_raw_boundaries_and_summary_count() {
        const CHUNK_SIZE: u64 = 4_096;
        let store = DerivedLanes::new();
        let lane = store.register("w", DerivedLaneData::Annotations(Vec::new()));
        store.append_word_batch(
            lane,
            (0..CHUNK_SIZE + 4).map(|index| (index * 10, 0, index)),
        );

        let lanes = store.read();
        let DerivedLaneData::Annotations(annotations) = &lanes[0].data else {
            panic!("expected annotations");
        };
        assert_eq!(annotations.len(), (CHUNK_SIZE + 4) as usize);
        assert_eq!(
            annotations[CHUNK_SIZE as usize - 1].end_ns,
            CHUNK_SIZE * 10,
            "the word crossing the summary chunk boundary remains exact"
        );

        let LaneSummary::Annotations(summary) = &lanes[0].summary else {
            panic!("expected annotation summary");
        };
        assert_eq!(summary.len(), (CHUNK_SIZE + 3) as usize);
        let records = summary.sampled_window(0, (CHUNK_SIZE + 4) * 10, 1);
        assert_eq!(
            records
                .iter()
                .map(|record| u64::from(record.count))
                .sum::<u64>(),
            CHUNK_SIZE + 3
        );
    }

    #[test]
    fn lane_growth_has_no_cap() {
        // Not a real-world entry count (that would just make the test
        // slow) — just enough to prove there's no hidden ceiling like the
        // old `MAX_LANE_ENTRIES` silently discarding past some threshold.
        const ENTRIES: u64 = 10_000;
        let store = DerivedLanes::new();
        let lane = store.register("m", DerivedLaneData::Markers(Vec::new()));
        store.append_marker_batch(lane, 0..ENTRIES);
        let lanes = store.read();
        let DerivedLaneData::Markers(markers) = &lanes[0].data else {
            panic!("expected markers");
        };
        assert_eq!(markers.len(), ENTRIES as usize);
        assert_eq!(markers.last(), Some(&(ENTRIES - 1)));
    }

    #[test]
    fn collector_retains_the_complete_timeline_by_default() {
        let sink = DerivedDataCollector::new(DerivedLanes::new());
        assert_eq!(sink.retention, DerivedDataRetention::Unlimited);
    }

    #[test]
    fn derived_data_retention_drops_oldest_exact_entries_but_keeps_full_summary() {
        let store = DerivedLanes::new();
        let mut sink = DerivedDataCollector::new(store.clone())
            .with_indexed_words(false)
            .with_retention(DerivedDataRetention::MaxEntries(4))
            .with_lane(CollectedDataKind::Words, "words");
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<Word>>(2);
        tx.send(ChannelMessage::Batch(
            (0..6).map(|index| Word::new(index, index * 100)).collect(),
        ))
        .unwrap();
        drop(tx);

        run_sink(
            &mut sink,
            vec![InputPort::new_with_watchdog(rx, &wd, "viewer", "in0")],
        );

        let lanes = store.read();
        let DerivedLaneData::Annotations(annotations) = &lanes[0].data else {
            panic!("expected annotations");
        };
        assert_eq!(
            annotations
                .iter()
                .map(|annotation| annotation.value)
                .collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        let LaneSummary::Annotations(summary) = &lanes[0].summary else {
            panic!("expected annotation summary");
        };
        assert_eq!(summary.len(), 5, "the newest annotation remains open");
        assert_eq!(summary.sampled_window(0, 1_000, 10)[0].start_ns, 0);
    }

    #[test]
    fn indexed_store_creation_failure_falls_back_to_in_memory_annotations() {
        let store = DerivedLanes::new();
        let config = LiveStoreConfig {
            hot_tail_publish_words: 0,
            ..LiveStoreConfig::default()
        };

        let sink = DerivedDataCollector::new(store.clone())
            .with_word_store_config(config)
            .with_lane(CollectedDataKind::Words, "words");

        assert!(matches!(
            &sink.lanes[0],
            CollectorLane::BuiltIn(lane) if lane.word_writer.is_none()
        ));
        assert!(matches!(
            store.read()[0].data,
            DerivedLaneData::Annotations(_)
        ));
    }

    #[test]
    fn indexed_lane_preserves_a_batch_larger_than_one_sink_drain() {
        let directory = tempfile::tempdir().unwrap();
        let store = DerivedLanes::new();
        let config = LiveStoreConfig {
            directory: directory.path().to_path_buf(),
            ..LiveStoreConfig::default()
        };
        let mut sink = DerivedDataCollector::new(store.clone())
            .with_word_store_config(config)
            .with_lane(CollectedDataKind::Words, "words");
        let word_count = DRAIN_BATCH_SIZE + 17;
        let words: Vec<_> = (0..word_count as u64)
            .map(|index| Word::new(index, index * 10))
            .collect();
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<Word>>(2);
        tx.send(ChannelMessage::Batch(words)).unwrap();
        drop(tx);

        run_sink(
            &mut sink,
            vec![InputPort::new_with_watchdog(rx, &wd, "viewer", "in0")],
        );

        let lanes = store.read();
        let DerivedLaneData::IndexedAnnotations(indexed) = &lanes[0].data else {
            panic!("expected indexed annotation lane");
        };
        assert_eq!(indexed.status(), StoreStatus::Finished);
        assert_eq!(indexed.metadata().total_word_count, word_count as u64);
        let tail = indexed
            .query
            .exact_window((word_count as u64 - 3) * 10, word_count as u64 * 10, 10)
            .unwrap();
        assert!(tail.complete);
        assert_eq!(
            tail.annotations.last().unwrap().value,
            word_count as u64 - 1
        );
    }

    #[test]
    fn indexed_lane_failure_does_not_stop_other_collected_lanes() {
        let directory = tempfile::tempdir().unwrap();
        let store = DerivedLanes::new();
        let config = LiveStoreConfig {
            directory: directory.path().to_path_buf(),
            ..LiveStoreConfig::default()
        };
        let mut sink = DerivedDataCollector::new(store.clone())
            .with_word_store_config(config)
            .with_lane(CollectedDataKind::Words, "words")
            .with_lane(CollectedDataKind::Trigger, "trigger");
        let wd = Watchdog::new();
        let (word_tx, word_rx) = bounded::<ChannelMessage<Word>>(4);
        word_tx
            .send(ChannelMessage::Batch(vec![
                Word::new(1, 10),
                Word::new(2, 5),
            ]))
            .unwrap();
        drop(word_tx);
        let (trigger_tx, trigger_rx) = bounded::<ChannelMessage<Trigger>>(4);
        trigger_tx
            .send(ChannelMessage::Sample(Trigger { timestamp_ns: 42 }))
            .unwrap();
        drop(trigger_tx);

        run_sink(
            &mut sink,
            vec![
                InputPort::new_with_watchdog(word_rx, &wd, "viewer", "in0"),
                InputPort::new_with_watchdog(trigger_rx, &wd, "viewer", "in1"),
            ],
        );

        let lanes = store.read();
        let DerivedLaneData::IndexedAnnotations(indexed) = &lanes[0].data else {
            panic!("expected indexed annotation lane");
        };
        assert!(matches!(indexed.status(), StoreStatus::Failed(_)));
        assert!(matches!(
            &lanes[1].data,
            DerivedLaneData::Markers(markers) if markers == &[42]
        ));
    }

    #[test]
    fn registering_a_new_indexed_writer_replaces_the_published_query_handle() {
        let directory = tempfile::tempdir().unwrap();
        let config = LiveStoreConfig {
            directory: directory.path().to_path_buf(),
            ..LiveStoreConfig::default()
        };
        let store = DerivedLanes::new();
        let first = DerivedDataCollector::new(store.clone())
            .with_word_store_config(config.clone())
            .with_lane(CollectedDataKind::Words, "words");
        let first_query = match &store.read()[0].data {
            DerivedLaneData::IndexedAnnotations(indexed) => Arc::clone(&indexed.query),
            other => panic!("expected indexed annotation lane, got {other:?}"),
        };

        let second = DerivedDataCollector::new(store.clone())
            .with_word_store_config(config)
            .with_lane(CollectedDataKind::Words, "words");
        let second_query = match &store.read()[0].data {
            DerivedLaneData::IndexedAnnotations(indexed) => Arc::clone(&indexed.query),
            other => panic!("expected indexed annotation lane, got {other:?}"),
        };

        assert!(!Arc::ptr_eq(&first_query, &second_query));
        drop((first, second));
        let lanes = store.read();
        let DerivedLaneData::IndexedAnnotations(indexed) = &lanes[0].data else {
            panic!("expected indexed annotation lane");
        };
        assert_eq!(indexed.status(), StoreStatus::Cancelled);
    }

    #[test]
    fn collector_reopens_persistent_lane_and_does_not_rewrite_incoming_words() {
        let directory = tempfile::tempdir().unwrap();
        let persistent =
            crate::derived_word_store::PersistentStoreConfig::new(directory.path(), [9; 32]);
        let config = LiveStoreConfig {
            directory: directory.path().to_path_buf(),
            persistence: Some(persistent),
            ..LiveStoreConfig::default()
        };
        let (mut writer, _) = IndexedAnnotationWriter::create(config.clone()).unwrap();
        writer
            .append_batch(&[Word::new(1, 10), Word::new(2, 20)])
            .unwrap();
        writer.finish().unwrap();
        drop(writer);

        let lanes = DerivedLanes::new();
        let mut sink = DerivedDataCollector::new(lanes.clone())
            .with_word_store_config(config)
            .with_lane(CollectedDataKind::Words, "words");
        assert!(matches!(
            &sink.lanes[0],
            CollectorLane::BuiltIn(lane) if lane.word_writer.is_none() && lane.word_indexed
        ));
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<Word>>(4);
        tx.send(ChannelMessage::Batch(vec![
            Word::new(99, 10),
            Word::new(100, 20),
        ]))
        .unwrap();
        drop(tx);
        run_sink(
            &mut sink,
            vec![InputPort::new_with_watchdog(rx, &wd, "viewer", "in0")],
        );

        let lanes = lanes.read();
        let DerivedLaneData::IndexedAnnotations(indexed) = &lanes[0].data else {
            panic!("expected indexed annotations");
        };
        assert_eq!(indexed.metadata().total_word_count, 2);
        assert_eq!(
            indexed.query.exact_window(0, 30, 10).unwrap().annotations[0].value,
            1
        );
    }
}
