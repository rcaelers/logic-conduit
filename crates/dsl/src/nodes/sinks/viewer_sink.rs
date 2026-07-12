//! Viewer sink: pushes decoded streams into a shared lane store the UI
//! renders as extra rows under the raw channels
//! (`docs/LOGIC_ANALYZER_VIEWER_DESIGN.md`).

use crate::runtime::derived_index::{AppendOnlyMipmap, ChunkedMipmap, LaneFold, MipmapRecord};
use crate::runtime::events::{Trigger, Word};
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};
use crate::runtime::sample::Sample;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use web_time::Instant;

#[derive(Clone, Default)]
pub struct ViewerSinkMetrics {
    inner: Arc<ViewerSinkMetricsInner>,
}

#[derive(Default)]
struct ViewerSinkMetricsInner {
    drain_ns: AtomicU64,
    append_ns: AtomicU64,
    items: AtomicU64,
    batches: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewerSinkMetricsSnapshot {
    pub drain_ns: u64,
    pub append_ns: u64,
    pub items: u64,
    pub batches: u64,
}

impl ViewerSinkMetrics {
    pub fn snapshot(&self) -> ViewerSinkMetricsSnapshot {
        ViewerSinkMetricsSnapshot {
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

/// Longest estimated box for the final open instantaneous annotation when no
/// next word exists to establish its real end. Closed instantaneous words
/// extend to the next word; words carrying a duration use their true extent.
pub const MAX_ANNOTATION_NS: u64 = 1_000_000;
/// Suggested per-lane limit for continuous sources that explicitly select
/// rolling exact-detail retention. The summary index retains the complete
/// timeline even when old raw entries are released.
pub const DEFAULT_VIEWER_MAX_ENTRIES: usize = 1_000_000;

/// Most items one lane drains from its channel per `work()` call. Bounds how
/// long one call holds `DerivedLanes`' write lock and, more importantly,
/// stops `ViewerSink` from racing a fast producer to keep its channel
/// perpetually empty — a channel that's allowed to actually fill is what
/// lets its `Block` overflow policy engage and slow the producer down.
const DRAIN_BATCH_SIZE: usize = 65_536;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ViewerRetention {
    #[default]
    Unlimited,
    MaxEntries(usize),
}

impl ViewerRetention {
    fn trim_target(self, len: usize) -> Option<usize> {
        let Self::MaxEntries(max) = self else {
            return None;
        };
        let max = max.max(1);
        (len > max).then_some((max - max / 4).max(1))
    }
}

/// A decoded word drawn as a labeled box. The label is formatted at render
/// time from `value` — storing strings per word would multiply the memory
/// cost of large captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Annotation {
    pub start_ns: u64,
    pub end_ns: u64,
    pub value: u64,
}

#[derive(Debug, Clone)]
pub enum DerivedLaneData {
    /// A boolean level stream, rendered like a channel waveform.
    Digital(Vec<Sample>),
    /// Word boxes. A word carrying a real duration is stored closed with
    /// its true `end_ns`; an instantaneous word's `end_ns` is patched to
    /// the next word's start as words arrive.
    Annotations(Vec<Annotation>),
    /// Zero-width event markers (trigger timestamps, ns).
    Markers(Vec<u64>),
}

/// How each lane kind folds into [`MipmapRecord`]s — see
/// `runtime::derived_index` for why this exists (a multi-resolution index
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

/// The multi-resolution index kept alongside a lane's raw `data`. It is
/// append-only and retains the complete timeline when a bounded retention
/// policy releases old exact entries.
#[derive(Debug, Clone)]
pub enum LaneSummary {
    Digital(AppendOnlyMipmap<Sample, DigitalFold>),
    Annotations(ChunkedMipmap<Annotation, AnnotationFold>),
    Markers(AppendOnlyMipmap<u64, MarkerFold>),
}

impl LaneSummary {
    /// A summary backfilled from `data` — every production caller registers
    /// a lane empty (`ViewerBuilder::build` always passes a fresh
    /// `Vec::new()`) so this is normally a no-op, but the invariant "summary
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
            DerivedLaneData::Markers(markers) => {
                let mut summary = AppendOnlyMipmap::new();
                summary.extend(markers);
                Self::Markers(summary)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct DerivedLane {
    pub name: String,
    pub data: DerivedLaneData,
    pub summary: LaneSummary,
}

/// Shared, append-only store of derived lanes. The compiler hands one clone
/// to every `ViewerSink` and one to the UI; a re-run swaps in a fresh store
/// so stale lanes vanish atomically.
#[derive(Debug, Clone, Default)]
pub struct DerivedLanes {
    inner: Arc<RwLock<Vec<DerivedLane>>>,
}

impl DerivedLanes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an empty lane and returns its index. Lane order is registration
    /// order (= viewer wiring order). Registering an existing name reuses
    /// that lane (keeping its data when the kind matches), so a viewer
    /// restarted in place by live reconfiguration continues its lanes
    /// instead of duplicating them.
    pub fn register(&self, name: impl Into<String>, data: DerivedLaneData) -> usize {
        let name = name.into();
        let mut lanes = self.inner.write().unwrap();
        if let Some(index) = lanes.iter().position(|lane| lane.name == name) {
            if std::mem::discriminant(&lanes[index].data) != std::mem::discriminant(&data) {
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
        });
        lanes.len() - 1
    }

    /// Read access for rendering.
    pub fn read(&self) -> RwLockReadGuard<'_, Vec<DerivedLane>> {
        self.inner.read().unwrap()
    }

    /// Appends a whole batch under a single write-lock acquisition — called
    /// once per `ViewerSink::work()` invocation per lane, rather than once
    /// per item, so a burst of decoded entries doesn't take (and contend
    /// the UI thread's `read()` for) the lock once per item.
    #[cfg(test)]
    fn append_digital_batch(&self, lane: usize, samples: impl IntoIterator<Item = Sample>) {
        self.append_digital_batch_retained(lane, samples, ViewerRetention::Unlimited);
    }

    fn append_digital_batch_retained(
        &self,
        lane: usize,
        samples: impl IntoIterator<Item = Sample>,
        retention: ViewerRetention,
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
    #[cfg(test)]
    fn append_word_batch(&self, lane: usize, words: impl IntoIterator<Item = (u64, u64, u64)>) {
        self.append_word_batch_retained(lane, words, ViewerRetention::Unlimited);
    }

    fn append_word_batch_retained(
        &self,
        lane: usize,
        words: impl IntoIterator<Item = (u64, u64, u64)>,
        retention: ViewerRetention,
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
            if let Some(previous) = annotations.last_mut()
                && previous.end_ns == previous.start_ns
            {
                previous.end_ns = start_ns;
                // Only now that its `end_ns` is final can it join the
                // summary — the mipmap is append-only and can never
                // retroactively patch an entry once it's folded into a
                // coarser tier, so the most recent annotation always lags
                // the summary by exactly one entry until the next word
                // closes it (or, if the run ends right after it, forever —
                // the raw `data` entry is still fully correct and is what
                // exact/near-zoom rendering reads directly; see
                // `draw_derived_annotations`'s open-ended handling).
                summary.push(previous);
            }
            let annotation = Annotation {
                start_ns,
                // A word with a real duration is closed right away at its
                // true end; an instantaneous one stays "open" (end ==
                // start) until the next word patches it above.
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

    #[cfg(test)]
    fn append_marker_batch(&self, lane: usize, timestamps: impl IntoIterator<Item = u64>) {
        self.append_marker_batch_retained(lane, timestamps, ViewerRetention::Unlimited);
    }

    fn append_marker_batch_retained(
        &self,
        lane: usize,
        timestamps: impl IntoIterator<Item = u64>,
        retention: ViewerRetention,
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
}

/// The three shapes a viewer lane's data can take
/// (`docs/LOGIC_ANALYZER_VIEWER_DESIGN.md`) — every decoder's output reduces to
/// one of these, so the viewer itself never needs to know which decoder
/// produced a lane: a level stream (`Signal`), a stream of decoded values
/// (`Words`, i.e. [`Word`]), or a stream of instantaneous events (`Trigger`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerLaneKind {
    Signal,
    Words,
    Trigger,
}

enum LaneBuffer {
    Signal(VecDeque<Sample>),
    Words(VecDeque<Word>),
    Trigger(VecDeque<Trigger>),
}

struct Lane {
    kind: ViewerLaneKind,
    store_index: usize,
    buffer: LaneBuffer,
    eos: bool,
}

/// Sink with one typed input per lane. Never blocks *waiting* on any single
/// input — lanes drain round-robin with `try_recv` so a quiet lane cannot
/// stall a busy one — but each lane's channel is drained in bounded batches
/// (`DRAIN_BATCH_SIZE`), not to exhaustion, so a channel that a producer is
/// filling faster than this sink drains it stays full and the producer's
/// own send genuinely blocks (`docs/PIPELINE_DESIGN.md`, flow control) — real
/// backpressure, not a silent drop once storage fills up.
pub struct ViewerSink {
    name: String,
    store: DerivedLanes,
    lanes: Vec<Lane>,
    retention: ViewerRetention,
    metrics: Option<ViewerSinkMetrics>,
}

impl ViewerSink {
    pub fn new(store: DerivedLanes) -> Self {
        Self {
            name: "viewer".to_string(),
            store,
            lanes: Vec::new(),
            retention: ViewerRetention::Unlimited,
            metrics: None,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_retention(mut self, retention: ViewerRetention) -> Self {
        self.retention = retention;
        self
    }

    pub fn with_metrics(mut self, metrics: ViewerSinkMetrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Appends a lane; input port order follows lane order (`in0`, `in1`, …).
    pub fn with_lane(mut self, kind: ViewerLaneKind, name: impl Into<String>) -> Self {
        let (data, buffer) = match kind {
            ViewerLaneKind::Signal => (
                DerivedLaneData::Digital(Vec::new()),
                LaneBuffer::Signal(VecDeque::new()),
            ),
            ViewerLaneKind::Words => (
                DerivedLaneData::Annotations(Vec::new()),
                LaneBuffer::Words(VecDeque::new()),
            ),
            ViewerLaneKind::Trigger => (
                DerivedLaneData::Markers(Vec::new()),
                LaneBuffer::Trigger(VecDeque::new()),
            ),
        };
        let store_index = self.store.register(name, data);
        self.lanes.push(Lane {
            kind,
            store_index,
            buffer,
            eos: false,
        });
        self
    }
}

impl ProcessNode for ViewerSink {
    fn name(&self) -> &str {
        &self.name
    }

    fn should_stop(&self) -> bool {
        !self.lanes.is_empty() && self.lanes.iter().all(|lane| lane.eos)
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
                match lane.kind {
                    ViewerLaneKind::Signal => {
                        PortSchema::new::<Sample>(name, index, PortDirection::Input)
                    }
                    ViewerLaneKind::Words => {
                        PortSchema::new::<Word>(name, index, PortDirection::Input)
                    }
                    ViewerLaneKind::Trigger => {
                        PortSchema::new::<Trigger>(name, index, PortDirection::Input)
                    }
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
            if lane.eos {
                continue;
            }
            let port = inputs
                .get(index)
                .ok_or_else(|| WorkError::NodeError(format!("Missing viewer input {index}")))?;

            // Bounded, not drained to exhaustion: letting a burst fill this
            // lane's channel (rather than racing to empty it every call) is
            // what makes the channel's own bound + `Block` overflow policy
            // (`docs/PIPELINE_DESIGN.md`, flow control) actually apply real
            // backpressure to the producer instead of never engaging.
            macro_rules! drain_batch {
                ($ty:ty, $buffer:expr) => {{
                    let drain_started = metrics.as_ref().map(|_| Instant::now());
                    let Some(mut receiver) = port.get::<$ty>($buffer) else {
                        // Unconnected input: nothing will ever arrive.
                        lane.eos = true;
                        continue;
                    };
                    let mut batch: Vec<$ty> = Vec::with_capacity(DRAIN_BATCH_SIZE);
                    match receiver.try_recv_many(&mut batch, DRAIN_BATCH_SIZE) {
                        Ok(_) | Err(TryRecvError::Empty) => {}
                        Err(TryRecvError::Disconnected) => lane.eos = true,
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
                        store.append_word_batch_retained(
                            lane.store_index,
                            batch
                                .into_iter()
                                .map(|w| (w.timestamp_ns, w.duration_ns, w.value)),
                            self.retention,
                        );
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
            }
        }

        if progress == 0 {
            if self.lanes.iter().all(|lane| lane.eos) {
                return Err(WorkError::Shutdown);
            }
            // All lanes momentarily quiet; don't spin.
            #[cfg(not(target_arch = "wasm32"))]
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::OutputPort as OutPort;
    use crate::runtime::sender::ChannelMessage;
    use crate::runtime::watchdog::Watchdog;
    use crossbeam_channel::bounded;

    fn run_sink(sink: &mut ViewerSink, inputs: Vec<InputPort>) {
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
    fn lanes_collect_signals_words_and_triggers() {
        let store = DerivedLanes::new();
        let mut sink = ViewerSink::new(store.clone())
            .with_lane(ViewerLaneKind::Signal, "latch.q")
            .with_lane(ViewerLaneKind::Words, "decoder.words")
            .with_lane(ViewerLaneKind::Trigger, "start.match");

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
        match &lanes[1].data {
            DerivedLaneData::Annotations(annotations) => {
                // First word's end patched to the second word's start.
                assert_eq!(
                    annotations.as_slice(),
                    &[
                        Annotation {
                            start_ns: 1_000,
                            end_ns: 1_500,
                            value: 0xAB
                        },
                        Annotation {
                            start_ns: 1_500,
                            end_ns: 1_500,
                            value: 0xCD
                        },
                    ]
                );
            }
            other => panic!("expected annotation lane, got {other:?}"),
        }
        match &lanes[2].data {
            DerivedLaneData::Markers(markers) => assert_eq!(markers.as_slice(), &[42]),
            other => panic!("expected marker lane, got {other:?}"),
        }
    }

    #[test]
    fn work_drains_at_most_one_batch_per_call() {
        // A single `work()` call must not race a fast producer to keep the
        // channel empty — that's what lets the channel's own bound and
        // `Block` overflow policy apply real backpressure instead of never
        // engaging (§`DRAIN_BATCH_SIZE`).
        let store = DerivedLanes::new();
        let mut sink = ViewerSink::new(store.clone()).with_lane(ViewerLaneKind::Signal, "sig");

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
    fn instantaneous_annotation_extends_to_next_word_across_long_gaps() {
        let store = DerivedLanes::new();
        let lane = store.register("w", DerivedLaneData::Annotations(Vec::new()));
        store.append_word_batch(
            lane,
            [(1_000, 0, 1), (1_000 + MAX_ANNOTATION_NS * 10, 0, 2)],
        );
        let lanes = store.read();
        let DerivedLaneData::Annotations(annotations) = &lanes[0].data else {
            panic!("expected annotations");
        };
        assert_eq!(annotations[0].end_ns, 1_000 + MAX_ANNOTATION_NS * 10);
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

    /// A word carrying a real duration (SPI, UART) is stored closed at its
    /// true end immediately — never patched to the next word's start, never
    /// left open for the renderer to estimate.
    #[test]
    fn word_with_duration_is_closed_at_its_true_end() {
        let store = DerivedLanes::new();
        let lane = store.register("w", DerivedLaneData::Annotations(Vec::new()));

        // A 24-bit SPI-like word spanning 2_300ns, followed much later by
        // another; the first's end must stay its own, not stretch to the
        // second's start.
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
    fn viewer_retains_the_complete_timeline_by_default() {
        let sink = ViewerSink::new(DerivedLanes::new());
        assert_eq!(sink.retention, ViewerRetention::Unlimited);
    }

    #[test]
    fn viewer_retention_drops_oldest_exact_entries_but_keeps_full_summary() {
        let store = DerivedLanes::new();
        let mut sink = ViewerSink::new(store.clone())
            .with_retention(ViewerRetention::MaxEntries(4))
            .with_lane(ViewerLaneKind::Words, "words");
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
}
