//! Viewer sink: pushes decoded streams into a shared lane store the UI
//! renders as extra rows under the raw channels
//! (`ANALYSIS_PIPELINE_DESIGN.md` §4.9).

use crate::nodes::decoders::{ParallelWord, SpiTransfer};
use crate::nodes::logic::{WordField, WordSource};
use crate::runtime::events::Trigger;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};
use crate::runtime::sample::Sample;
use std::collections::VecDeque;
use std::sync::{Arc, RwLock, RwLockReadGuard};

/// Hard cap per lane; decoded words can reach tens of millions on long
/// captures, which would otherwise eat gigabytes. Overflow increments
/// `DerivedLane::dropped` so the UI can show a truncation marker.
pub const MAX_LANE_ENTRIES: usize = 2_000_000;

/// Longest box a word annotation may span when its end is inferred from the
/// next word: keeps the last word of a burst from stretching across the idle
/// gap to the next one.
pub const MAX_ANNOTATION_NS: u64 = 1_000_000;

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

#[derive(Debug, Clone)]
pub struct DerivedLane {
    pub name: String,
    pub data: DerivedLaneData,
    /// Entries discarded after [`MAX_LANE_ENTRIES`] was reached.
    pub dropped: u64,
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
                lanes[index].data = data;
                lanes[index].dropped = 0;
            }
            return index;
        }
        lanes.push(DerivedLane {
            name,
            data,
            dropped: 0,
        });
        lanes.len() - 1
    }

    /// Read access for rendering.
    pub fn read(&self) -> RwLockReadGuard<'_, Vec<DerivedLane>> {
        self.inner.read().unwrap()
    }

    fn append_digital(&self, lane: usize, sample: Sample) {
        let mut lanes = self.inner.write().unwrap();
        let Some(lane) = lanes.get_mut(lane) else {
            return;
        };
        if let DerivedLaneData::Digital(samples) = &mut lane.data {
            if samples.len() >= MAX_LANE_ENTRIES {
                lane.dropped += 1;
                return;
            }
            samples.push(sample);
        }
    }

    fn append_word(&self, lane: usize, start_ns: u64, value: u64) {
        let mut lanes = self.inner.write().unwrap();
        let Some(lane) = lanes.get_mut(lane) else {
            return;
        };
        if let DerivedLaneData::Annotations(annotations) = &mut lane.data {
            if let Some(previous) = annotations.last_mut()
                && previous.end_ns == previous.start_ns
            {
                previous.end_ns = start_ns.min(previous.start_ns + MAX_ANNOTATION_NS);
            }
            if annotations.len() >= MAX_LANE_ENTRIES {
                lane.dropped += 1;
                return;
            }
            annotations.push(Annotation {
                start_ns,
                end_ns: start_ns,
                value,
            });
        }
    }

    fn append_marker(&self, lane: usize, timestamp_ns: u64) {
        let mut lanes = self.inner.write().unwrap();
        let Some(lane) = lanes.get_mut(lane) else {
            return;
        };
        if let DerivedLaneData::Markers(markers) = &mut lane.data {
            if markers.len() >= MAX_LANE_ENTRIES {
                lane.dropped += 1;
                return;
            }
            markers.push(timestamp_ns);
        }
    }
}

/// Stream kind of one viewer lane; decides the input port type and the lane
/// data representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerLaneKind {
    Signal,
    SpiWords,
    ParallelWords,
    Trigger,
}

enum LaneBuffer {
    Signal(VecDeque<Sample>),
    Spi(VecDeque<SpiTransfer>),
    Parallel(VecDeque<ParallelWord>),
    Trigger(VecDeque<Trigger>),
}

struct Lane {
    kind: ViewerLaneKind,
    store_index: usize,
    buffer: LaneBuffer,
    eos: bool,
}

/// Sink with one typed input per lane. Never blocks on any single input —
/// lanes drain round-robin with `try_recv` so a quiet lane cannot stall a
/// busy one — and never applies backpressure beyond its (bounded) input
/// buffers filling.
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

    /// Appends a lane; input port order follows lane order (`in0`, `in1`, …).
    pub fn with_lane(mut self, kind: ViewerLaneKind, name: impl Into<String>) -> Self {
        let data = match kind {
            ViewerLaneKind::Signal => DerivedLaneData::Digital(Vec::new()),
            ViewerLaneKind::SpiWords | ViewerLaneKind::ParallelWords => {
                DerivedLaneData::Annotations(Vec::new())
            }
            ViewerLaneKind::Trigger => DerivedLaneData::Markers(Vec::new()),
        };
        let store_index = self.store.register(name, data);
        let buffer = match kind {
            ViewerLaneKind::Signal => LaneBuffer::Signal(VecDeque::new()),
            ViewerLaneKind::SpiWords => LaneBuffer::Spi(VecDeque::new()),
            ViewerLaneKind::ParallelWords => LaneBuffer::Parallel(VecDeque::new()),
            ViewerLaneKind::Trigger => LaneBuffer::Trigger(VecDeque::new()),
        };
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
                    ViewerLaneKind::SpiWords => {
                        PortSchema::new::<SpiTransfer>(name, index, PortDirection::Input)
                    }
                    ViewerLaneKind::ParallelWords => {
                        PortSchema::new::<ParallelWord>(name, index, PortDirection::Input)
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
        let mut progress = 0usize;

        for (index, lane) in self.lanes.iter_mut().enumerate() {
            if lane.eos {
                continue;
            }
            let port = inputs
                .get(index)
                .ok_or_else(|| WorkError::NodeError(format!("Missing viewer input {index}")))?;

            macro_rules! drain {
                ($ty:ty, $buffer:expr, $append:expr) => {{
                    let Some(mut receiver) = port.get::<$ty>($buffer) else {
                        // Unconnected input: nothing will ever arrive.
                        lane.eos = true;
                        continue;
                    };
                    loop {
                        match receiver.try_recv() {
                            Ok(item) => {
                                $append(&store, lane.store_index, item);
                                progress += 1;
                            }
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => {
                                lane.eos = true;
                                break;
                            }
                        }
                    }
                }};
            }

            match &mut lane.buffer {
                LaneBuffer::Signal(buffer) => {
                    drain!(
                        Sample,
                        buffer,
                        |store: &DerivedLanes, lane, item: Sample| {
                            store.append_digital(lane, item)
                        }
                    )
                }
                LaneBuffer::Spi(buffer) => {
                    drain!(
                        SpiTransfer,
                        buffer,
                        |store: &DerivedLanes, lane, item: SpiTransfer| {
                            store.append_word(lane, item.timestamp_ns(), item.word(WordField::Mosi))
                        }
                    )
                }
                LaneBuffer::Parallel(buffer) => {
                    drain!(
                        ParallelWord,
                        buffer,
                        |store: &DerivedLanes, lane, item: ParallelWord| {
                            store.append_word(lane, item.timestamp_ns(), item.word(WordField::Mosi))
                        }
                    )
                }
                LaneBuffer::Trigger(buffer) => {
                    drain!(
                        Trigger,
                        buffer,
                        |store: &DerivedLanes, lane, item: Trigger| {
                            store.append_marker(lane, item.timestamp_ns)
                        }
                    )
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
            .with_lane(ViewerLaneKind::ParallelWords, "decoder.words")
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
    fn annotation_end_is_capped_across_gaps() {
        let store = DerivedLanes::new();
        let lane = store.register("w", DerivedLaneData::Annotations(Vec::new()));
        store.append_word(lane, 1_000, 1);
        store.append_word(lane, 1_000 + MAX_ANNOTATION_NS * 10, 2);
        let lanes = store.read();
        let DerivedLaneData::Annotations(annotations) = &lanes[0].data else {
            panic!("expected annotations");
        };
        assert_eq!(annotations[0].end_ns, 1_000 + MAX_ANNOTATION_NS);
    }

    #[test]
    fn lane_cap_counts_dropped_entries() {
        let store = DerivedLanes::new();
        let lane = store.register("m", DerivedLaneData::Markers(Vec::new()));
        for ts in 0..(MAX_LANE_ENTRIES as u64 + 5) {
            store.append_marker(lane, ts);
        }
        let lanes = store.read();
        let DerivedLaneData::Markers(markers) = &lanes[0].data else {
            panic!("expected markers");
        };
        assert_eq!(markers.len(), MAX_LANE_ENTRIES);
        assert_eq!(lanes[0].dropped, 5);
    }
}
