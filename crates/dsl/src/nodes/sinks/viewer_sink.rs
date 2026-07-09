//! Viewer sink: pushes decoded streams into a shared lane store the UI
//! renders as extra rows under the raw channels
//! (`ANALYSIS_PIPELINE_DESIGN.md` §4.9).

use crate::nodes::logic::{WordField, WordSource};
use crate::runtime::derived_index::{AppendOnlyMipmap, LaneFold, MipmapRecord};
use crate::runtime::events::Trigger;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};
use crate::runtime::sample::Sample;
use std::collections::VecDeque;
use std::sync::{Arc, RwLock, RwLockReadGuard};

/// Longest box a word annotation may span when its end is inferred from the
/// next word: keeps the last word of a burst from stretching across the idle
/// gap to the next one.
pub const MAX_ANNOTATION_NS: u64 = 1_000_000;

/// Most items one lane drains from its channel per `work()` call. Bounds how
/// long one call holds `DerivedLanes`' write lock and, more importantly,
/// stops `ViewerSink` from racing a fast producer to keep its channel
/// perpetually empty — a channel that's allowed to actually fill is what
/// lets its `Block` overflow policy engage and slow the producer down.
const DRAIN_BATCH_SIZE: usize = 256;

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
    /// Word boxes; `end_ns` is patched to the next word's start (capped at
    /// [`MAX_ANNOTATION_NS`]) as words arrive.
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
            start_ns: entry.start_time,
            end_ns: entry.start_time,
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

/// The multi-resolution index kept alongside a lane's raw `data`, mirroring
/// its shape one-for-one — updated by the same `append_*_batch` calls, so
/// it's never out of sync with what's actually stored.
#[derive(Debug, Clone)]
pub enum LaneSummary {
    Digital(AppendOnlyMipmap<Sample, DigitalFold>),
    Annotations(AppendOnlyMipmap<Annotation, AnnotationFold>),
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
                let mut summary = AppendOnlyMipmap::new();
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
    fn append_digital_batch(&self, lane: usize, samples: impl IntoIterator<Item = Sample>) {
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
    }

    fn append_word_batch(&self, lane: usize, words: impl IntoIterator<Item = (u64, u64)>) {
        let mut lanes = self.inner.write().unwrap();
        let Some(lane) = lanes.get_mut(lane) else {
            return;
        };
        let (DerivedLaneData::Annotations(annotations), LaneSummary::Annotations(summary)) =
            (&mut lane.data, &mut lane.summary)
        else {
            return;
        };
        for (start_ns, value) in words {
            if let Some(previous) = annotations.last_mut()
                && previous.end_ns == previous.start_ns
            {
                previous.end_ns = start_ns.min(previous.start_ns + MAX_ANNOTATION_NS);
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
            annotations.push(Annotation {
                start_ns,
                end_ns: start_ns,
                value,
            });
        }
    }

    fn append_marker_batch(&self, lane: usize, timestamps: impl IntoIterator<Item = u64>) {
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
    }
}

/// The three shapes a viewer lane's data can take
/// (`ANALYSIS_PIPELINE_DESIGN.md` §4.9) — every decoder's output reduces to
/// one of these. The viewer itself never needs to know which decoder (or
/// concrete word-carrying Rust type) produced a lane: a level stream
/// (`Signal`), a stream of decoded values (`Words` — built via
/// [`ViewerSink::with_words_lane`], generic over any `T: WordSource`; the
/// concrete `T` is a choice the *caller* makes, not something this type
/// names), or a stream of instantaneous events (`Trigger`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerLaneKind {
    Signal,
    Words,
    Trigger,
}

/// Type-erased per-lane word decoder: knows this lane's `PortSchema` and how
/// to drain a batch of `(timestamp_ns, word)` pairs from its input, for
/// whatever concrete `T: WordSource` it was built with in
/// [`ViewerSink::with_words_lane`]. Keeps every decoder's word type out of
/// [`ViewerLaneKind`]/[`LaneBuffer`] — they only ever see "a `Words` lane".
trait WordDrain: Send {
    fn port_schema(&self, name: String, index: usize) -> PortSchema;
    /// Drains up to `DRAIN_BATCH_SIZE` items, returning `(timestamp_ns,
    /// word)` pairs and whether the channel is now known to be closed.
    fn drain(&mut self, port: &InputPort) -> (Vec<(u64, u64)>, bool);
}

struct TypedWordBuffer<T> {
    buffer: VecDeque<T>,
}

impl<T: WordSource> WordDrain for TypedWordBuffer<T> {
    fn port_schema(&self, name: String, index: usize) -> PortSchema {
        PortSchema::new::<T>(name, index, PortDirection::Input)
    }

    fn drain(&mut self, port: &InputPort) -> (Vec<(u64, u64)>, bool) {
        use crossbeam_channel::TryRecvError;
        let Some(mut receiver) = port.get::<T>(&mut self.buffer) else {
            return (Vec::new(), true);
        };
        let mut items = Vec::new();
        let mut eos = false;
        while items.len() < DRAIN_BATCH_SIZE {
            match receiver.try_recv() {
                Ok(item) => items.push((item.timestamp_ns(), item.word(WordField::Mosi))),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    eos = true;
                    break;
                }
            }
        }
        (items, eos)
    }
}

enum LaneBuffer {
    Signal(VecDeque<Sample>),
    Words(Box<dyn WordDrain>),
    Trigger(VecDeque<Trigger>),
}

struct Lane {
    store_index: usize,
    buffer: LaneBuffer,
    eos: bool,
}

/// Sink with one typed input per lane. Never blocks *waiting* on any single
/// input — lanes drain round-robin with `try_recv` so a quiet lane cannot
/// stall a busy one — but each lane's channel is drained in bounded batches
/// (`DRAIN_BATCH_SIZE`), not to exhaustion, so a channel that a producer is
/// filling faster than this sink drains it stays full and the producer's
/// own send genuinely blocks (`ANALYSIS_PIPELINE_DESIGN.md` §6.4) — real
/// backpressure, not a silent drop once storage fills up.
pub struct ViewerSink {
    name: String,
    store: DerivedLanes,
    lanes: Vec<Lane>,
}

impl ViewerSink {
    pub fn new(store: DerivedLanes) -> Self {
        Self {
            name: "viewer".to_string(),
            store,
            lanes: Vec::new(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Appends a `Signal` or `Trigger` lane; input port order follows lane
    /// order (`in0`, `in1`, …). Use [`Self::with_words_lane`] for `Words`.
    pub fn with_lane(mut self, kind: ViewerLaneKind, name: impl Into<String>) -> Self {
        let (data, buffer) = match kind {
            ViewerLaneKind::Signal => (
                DerivedLaneData::Digital(Vec::new()),
                LaneBuffer::Signal(VecDeque::new()),
            ),
            ViewerLaneKind::Trigger => (
                DerivedLaneData::Markers(Vec::new()),
                LaneBuffer::Trigger(VecDeque::new()),
            ),
            ViewerLaneKind::Words => {
                panic!("with_lane doesn't support Words — use with_words_lane::<T>()")
            }
        };
        let store_index = self.store.register(name, data);
        self.lanes.push(Lane {
            store_index,
            buffer,
            eos: false,
        });
        self
    }

    /// Appends a `Words` lane carrying `T`. Generic so this file never needs
    /// to name a specific decoder's word type — the caller (the compiler's
    /// `ViewerBuilder`) picks `T` from the negotiated `PortKind`.
    pub fn with_words_lane<T: WordSource>(mut self, name: impl Into<String>) -> Self {
        let store_index = self
            .store
            .register(name, DerivedLaneData::Annotations(Vec::new()));
        self.lanes.push(Lane {
            store_index,
            buffer: LaneBuffer::Words(Box::new(TypedWordBuffer::<T> {
                buffer: VecDeque::new(),
            })),
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
                match &lane.buffer {
                    LaneBuffer::Signal(_) => {
                        PortSchema::new::<Sample>(name, index, PortDirection::Input)
                    }
                    LaneBuffer::Words(word_drain) => word_drain.port_schema(name, index),
                    LaneBuffer::Trigger(_) => {
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
            // (`ANALYSIS_PIPELINE_DESIGN.md` §6.4) actually apply real
            // backpressure to the producer instead of never engaging.
            macro_rules! drain_batch {
                ($ty:ty, $buffer:expr) => {{
                    let Some(mut receiver) = port.get::<$ty>($buffer) else {
                        // Unconnected input: nothing will ever arrive.
                        lane.eos = true;
                        continue;
                    };
                    let mut batch: Vec<$ty> = Vec::new();
                    while batch.len() < DRAIN_BATCH_SIZE {
                        match receiver.try_recv() {
                            Ok(item) => batch.push(item),
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => {
                                lane.eos = true;
                                break;
                            }
                        }
                    }
                    batch
                }};
            }

            match &mut lane.buffer {
                LaneBuffer::Signal(buffer) => {
                    let batch = drain_batch!(Sample, buffer);
                    progress += batch.len();
                    if !batch.is_empty() {
                        store.append_digital_batch(lane.store_index, batch);
                    }
                }
                LaneBuffer::Words(word_drain) => {
                    let (items, eos) = word_drain.drain(port);
                    progress += items.len();
                    if eos {
                        lane.eos = true;
                    }
                    if !items.is_empty() {
                        store.append_word_batch(lane.store_index, items);
                    }
                }
                LaneBuffer::Trigger(buffer) => {
                    let batch = drain_batch!(Trigger, buffer);
                    progress += batch.len();
                    if !batch.is_empty() {
                        store.append_marker_batch(
                            lane.store_index,
                            batch.into_iter().map(|item| item.timestamp_ns),
                        );
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
    use crate::TimingInfo;
    use crate::nodes::decoders::ParallelWord;
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
            .with_words_lane::<ParallelWord>("decoder.words")
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

        let (word_tx, word_rx) = bounded::<ChannelMessage<ParallelWord>>(16);
        for (value, ts) in [(0xAB_u64, 1_000_u64), (0xCD, 1_500)] {
            word_tx
                .send(ChannelMessage::Sample(ParallelWord {
                    value,
                    timing: TimingInfo::new(ts as f64 / 1_000.0, ts),
                }))
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
    fn annotation_end_is_capped_across_gaps() {
        let store = DerivedLanes::new();
        let lane = store.register("w", DerivedLaneData::Annotations(Vec::new()));
        store.append_word_batch(lane, [(1_000, 1), (1_000 + MAX_ANNOTATION_NS * 10, 2)]);
        let lanes = store.read();
        let DerivedLaneData::Annotations(annotations) = &lanes[0].data else {
            panic!("expected annotations");
        };
        assert_eq!(annotations[0].end_ns, 1_000 + MAX_ANNOTATION_NS);
    }

    #[test]
    fn summary_tracks_digital_samples_as_they_arrive() {
        let store = DerivedLanes::new();
        let lane = store.register("d", DerivedLaneData::Digital(Vec::new()));
        store.append_digital_batch(
            lane,
            [Sample::new(true, 100), Sample::new(false, 300)],
        );
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

    #[test]
    fn summary_lags_the_most_recent_open_annotation_by_one() {
        // The mipmap can't retroactively patch an entry once it's pushed,
        // so the most recent (still "open", not yet end-patched) annotation
        // only joins the summary once the *next* word closes it.
        let store = DerivedLanes::new();
        let lane = store.register("w", DerivedLaneData::Annotations(Vec::new()));

        store.append_word_batch(lane, [(1_000, 0xAB)]);
        {
            let lanes = store.read();
            let LaneSummary::Annotations(summary) = &lanes[0].summary else {
                panic!("expected an annotations summary");
            };
            assert_eq!(summary.len(), 0, "the only word so far is still open");
        }

        store.append_word_batch(lane, [(1_500, 0xCD)]);
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
}
