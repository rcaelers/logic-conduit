use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::{
    CaptureCursorItem, CaptureIndex, CaptureMetadata, CaptureSampledChannel, CaptureSampledWindow,
    CaptureStoreCursor, CaptureWaveformSegment, Error, NativeCaptureRandomReader,
    NativeCaptureStore, Result, exact_window_sample_limit,
};

const LEAF_SAMPLES: u64 = 64;
const FAN_OUT: usize = 64;
const QUERY_WAIT: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WaveformRecord {
    start_sample: u64,
    end_sample: u64,
    first: bool,
    last: bool,
    activity: bool,
}

impl WaveformRecord {
    fn combine(records: &[Self]) -> Self {
        let first = records[0];
        let last = records[records.len() - 1];
        let boundary_activity = records
            .windows(2)
            .any(|pair| pair[0].last != pair[1].first);
        Self {
            start_sample: first.start_sample,
            end_sample: last.end_sample,
            first: first.first,
            last: last.last,
            activity: boundary_activity || records.iter().any(|record| record.activity),
        }
    }
}

#[derive(Debug, Default)]
struct WaveformMipmap {
    tiers: Vec<Vec<WaveformRecord>>,
    leaves: usize,
}

impl WaveformMipmap {
    fn push(&mut self, record: WaveformRecord) {
        self.push_at(0, record);
        self.leaves += 1;
    }

    fn push_at(&mut self, tier: usize, record: WaveformRecord) {
        if tier == self.tiers.len() {
            self.tiers.push(Vec::new());
        }
        self.tiers[tier].push(record);
        if self.tiers[tier].len().is_multiple_of(FAN_OUT) {
            let start = self.tiers[tier].len() - FAN_OUT;
            let combined = WaveformRecord::combine(&self.tiers[tier][start..]);
            self.push_at(tier + 1, combined);
        }
    }

    fn sampled_window(
        &self,
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
        tail: Option<WaveformRecord>,
    ) -> Vec<WaveformRecord> {
        let budget = target_points.max(1).saturating_mul(2);
        for tier_index in (0..self.tiers.len()).rev() {
            let tier = &self.tiers[tier_index];
            let (first, last) = window_range(tier, start_sample, end_sample);
            if last - first <= budget || tier_index == 0 {
                let mut result = tier[first..last].to_vec();
                self.append_uncovered_tail(
                    tier_index,
                    start_sample,
                    end_sample,
                    &mut result,
                );
                if let Some(tail) = tail
                    && tail.end_sample > start_sample
                    && tail.start_sample < end_sample
                {
                    result.push(tail);
                }
                return result;
            }
        }
        tail.into_iter()
            .filter(|tail| tail.end_sample > start_sample && tail.start_sample < end_sample)
            .collect()
    }

    fn append_uncovered_tail(
        &self,
        tier_index: usize,
        start_sample: u64,
        end_sample: u64,
        output: &mut Vec<WaveformRecord>,
    ) {
        if tier_index == 0 || self.tiers.is_empty() {
            return;
        }
        let Some(group_size) = FAN_OUT.checked_pow(tier_index as u32) else {
            return;
        };
        let covered = self.tiers[tier_index].len().saturating_mul(group_size);
        if covered >= self.leaves {
            return;
        }
        let raw = &self.tiers[0][covered..];
        let (first, last) = window_range(raw, start_sample, end_sample);
        if last > first {
            output.push(WaveformRecord::combine(&raw[first..last]));
        }
    }
}

fn window_range(
    records: &[WaveformRecord],
    start_sample: u64,
    end_sample: u64,
) -> (usize, usize) {
    let first = records.partition_point(|record| record.end_sample <= start_sample);
    let last = first
        + records[first..].partition_point(|record| record.start_sample < end_sample);
    (first, last)
}

#[derive(Clone, Copy, Debug)]
struct ActiveRecord {
    start_sample: u64,
    first: bool,
    last: bool,
    activity: bool,
}

impl ActiveRecord {
    fn new(sample: u64, value: bool) -> Self {
        Self {
            start_sample: sample,
            first: value,
            last: value,
            activity: false,
        }
    }

    fn push(&mut self, value: bool) {
        self.activity |= value != self.last;
        self.last = value;
    }

    fn finish(self, end_sample: u64) -> WaveformRecord {
        WaveformRecord {
            start_sample: self.start_sample,
            end_sample,
            first: self.first,
            last: self.last,
            activity: self.activity,
        }
    }
}

struct SummaryBuilder {
    active: Vec<Option<ActiveRecord>>,
    next_sample: u64,
}

impl SummaryBuilder {
    fn new(channels: usize) -> Self {
        Self {
            active: vec![None; channels],
            next_sample: 0,
        }
    }

    fn append_chunk(&mut self, chunk: &crate::CaptureChunk) -> Result<Vec<Vec<WaveformRecord>>> {
        if chunk.start_sample() != self.next_sample || chunk.channels().len() != self.active.len() {
            return Err(Error::ParseError(
                "growing waveform received a discontinuous capture chunk".into(),
            ));
        }
        let mut completed = vec![Vec::new(); self.active.len()];
        for relative in 0..chunk.sample_count() {
            let sample = chunk.start_sample() + relative;
            for (channel, active) in self.active.iter_mut().enumerate() {
                let value = chunk
                    .packed_level(relative, channel)
                    .expect("validated capture chunk contains every channel sample");
                match active {
                    Some(record) => record.push(value),
                    None => *active = Some(ActiveRecord::new(sample, value)),
                }
            }
            let end_sample = sample + 1;
            if end_sample.is_multiple_of(LEAF_SAMPLES) {
                for (channel, active) in self.active.iter_mut().enumerate() {
                    completed[channel].push(
                        active
                            .take()
                            .expect("every channel has an active summary")
                            .finish(end_sample),
                    );
                }
            }
        }
        self.next_sample = chunk.end_sample();
        Ok(completed)
    }

    fn active_records(&self) -> Vec<Option<WaveformRecord>> {
        self.active
            .iter()
            .map(|active| active.map(|record| record.finish(self.next_sample)))
            .collect()
    }

    fn finish(&mut self) -> Vec<Vec<WaveformRecord>> {
        let mut completed = vec![Vec::new(); self.active.len()];
        for (channel, active) in self.active.iter_mut().enumerate() {
            if let Some(active) = active.take() {
                completed[channel].push(active.finish(self.next_sample));
            }
        }
        completed
    }
}

struct GrowingState {
    channels: Vec<WaveformMipmap>,
    tails: Vec<Option<WaveformRecord>>,
    indexed_samples: u64,
    committed_chunks: u64,
    generation: u64,
    complete: bool,
    error: Option<String>,
}

impl GrowingState {
    fn new(channels: usize) -> Self {
        Self {
            channels: (0..channels).map(|_| WaveformMipmap::default()).collect(),
            tails: vec![None; channels],
            indexed_samples: 0,
            committed_chunks: 0,
            generation: 0,
            complete: false,
            error: None,
        }
    }

    fn publish(
        &mut self,
        completed: Vec<Vec<WaveformRecord>>,
        tails: Vec<Option<WaveformRecord>>,
        indexed_samples: u64,
    ) {
        for (mipmap, records) in self.channels.iter_mut().zip(completed) {
            for record in records {
                mipmap.push(record);
            }
        }
        self.tails = tails;
        self.indexed_samples = indexed_samples;
        self.committed_chunks += 1;
        self.generation = self.generation.wrapping_add(1);
    }
}

/// Cloneable growing query handle. Its background owner follows a committed
/// store cursor; clones only read published summaries or exact raw windows.
pub struct NativeGrowingCaptureIndex {
    display_name: String,
    index_path: PathBuf,
    header: CaptureMetadata,
    store: NativeCaptureStore,
    state: Arc<RwLock<GrowingState>>,
    random_reader: Option<NativeCaptureRandomReader>,
}

impl Clone for NativeGrowingCaptureIndex {
    fn clone(&self) -> Self {
        Self {
            display_name: self.display_name.clone(),
            index_path: self.index_path.clone(),
            header: self.header.clone(),
            store: self.store.clone(),
            state: Arc::clone(&self.state),
            random_reader: None,
        }
    }
}

impl NativeGrowingCaptureIndex {
    pub fn spawn(
        store: NativeCaptureStore,
        display_name: impl Into<String>,
        sample_rate_hz: f64,
        probe_names: Vec<String>,
    ) -> Result<(Self, NativeGrowingCaptureIndexWorker)> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err(Error::ParseError(
                "live capture sample rate must be positive".into(),
            ));
        }
        if probe_names.len() != store.descriptor().channels().len() {
            return Err(Error::ParseError(
                "live capture channel names do not match its channel table".into(),
            ));
        }
        let header = CaptureMetadata {
            total_probes: probe_names.len(),
            samplerate: format_sample_rate(sample_rate_hz),
            samplerate_hz: sample_rate_hz,
            sample_period: 1.0 / sample_rate_hz,
            total_samples: 0,
            total_blocks: 0,
            samples_per_block: LEAF_SAMPLES,
            probe_names,
        };
        let state = Arc::new(RwLock::new(GrowingState::new(header.total_probes)));
        let cursor = store
            .open_cursor()
            .map_err(|error| Error::ParseError(error.to_string()))?;
        let worker_state = Arc::clone(&state);
        let channels = header.total_probes;
        let handle = std::thread::Builder::new()
            .name("live-waveform-index".into())
            .spawn(move || run_index_worker(cursor, worker_state, channels))?;
        let query = Self {
            display_name: display_name.into(),
            index_path: store.directory().join("capture.commits"),
            header,
            store,
            state,
            random_reader: None,
        };
        Ok((query, NativeGrowingCaptureIndexWorker { handle: Some(handle) }))
    }

    fn snapshot_metadata(&self) -> CaptureMetadata {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        let mut metadata = self.header.clone();
        metadata.total_samples = state.indexed_samples;
        metadata.total_blocks = state.committed_chunks;
        metadata
    }

    fn exact_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
    ) -> Result<CaptureSampledWindow> {
        if self.random_reader.is_none() {
            self.random_reader = Some(
                self.store
                    .open_random_reader()
                    .map_err(|error| Error::ParseError(error.to_string()))?,
            );
        }
        let mut window = self
            .random_reader
            .as_mut()
            .expect("random reader was initialized")
            .sampled_window(channels, start_sample, end_sample)?;
        for channel in &mut window.channels {
            channel.name = self.header.probe_names[channel.channel].clone();
        }
        Ok(window)
    }

    fn summary_window(
        &self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> Result<CaptureSampledWindow> {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        if let Some(error) = &state.error {
            return Err(Error::ParseError(error.clone()));
        }
        let mut sampled = Vec::with_capacity(channels.len());
        let mut sample_step = 1_u64;
        for &channel in channels {
            let Some(mipmap) = state.channels.get(channel) else {
                return Err(Error::InvalidProbe(channel));
            };
            let records = mipmap.sampled_window(
                start_sample,
                end_sample,
                target_points,
                state.tails[channel],
            );
            sample_step = sample_step.max(
                records
                    .iter()
                    .map(|record| record.end_sample - record.start_sample)
                    .max()
                    .unwrap_or(1),
            );
            let initial = records.first().is_some_and(|record| record.first);
            sampled.push(CaptureSampledChannel {
                channel,
                name: self.header.probe_names[channel].clone(),
                initial,
                transitions: Vec::new(),
                waveform: records_to_segments(&records, start_sample, end_sample),
            });
        }
        Ok(CaptureSampledWindow {
            start_sample,
            end_sample,
            sample_step,
            channels: sampled,
        })
    }
}

impl CaptureIndex for NativeGrowingCaptureIndex {
    fn display_name(&self) -> String {
        self.display_name.clone()
    }

    fn index_path(&self) -> &Path {
        &self.index_path
    }

    fn header(&self) -> &CaptureMetadata {
        &self.header
    }

    fn current_metadata(&self) -> CaptureMetadata {
        self.snapshot_metadata()
    }

    fn generation(&self) -> u64 {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .generation
    }

    fn is_complete(&self) -> bool {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .complete
    }

    fn capture_duration_us(&self) -> f64 {
        self.snapshot_metadata().duration_us()
    }

    fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> Result<CaptureSampledWindow> {
        let metadata = self.snapshot_metadata();
        if metadata.total_samples == 0 {
            return Err(Error::OutOfBounds(end_sample));
        }
        let start_sample = start_sample.min(metadata.total_samples - 1);
        let end_sample = end_sample.clamp(start_sample + 1, metadata.total_samples);
        if end_sample - start_sample <= exact_window_sample_limit(target_points) {
            self.exact_window(channels, start_sample, end_sample)
        } else {
            self.summary_window(channels, start_sample, end_sample, target_points)
        }
    }
}

pub struct NativeGrowingCaptureIndexWorker {
    handle: Option<JoinHandle<Result<()>>>,
}

impl NativeGrowingCaptureIndexWorker {
    pub fn join(mut self) -> Result<()> {
        self.handle
            .take()
            .expect("live waveform worker is joined once")
            .join()
            .map_err(|_| Error::ParseError("live waveform index worker panicked".into()))?
    }
}

fn run_index_worker(
    mut cursor: crate::NativeCaptureCursor,
    state: Arc<RwLock<GrowingState>>,
    channels: usize,
) -> Result<()> {
    let mut builder = SummaryBuilder::new(channels);
    loop {
        match cursor
            .wait_next(QUERY_WAIT)
            .map_err(|error| Error::ParseError(error.to_string()))
        {
            Ok(CaptureCursorItem::Chunk(chunk)) => {
                let completed = builder.append_chunk(&chunk)?;
                let tails = builder.active_records();
                state
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .publish(completed, tails, chunk.end_sample());
            }
            Ok(CaptureCursorItem::Pending) => {}
            Ok(CaptureCursorItem::End) => {
                let completed = builder.finish();
                let mut state = state.write().unwrap_or_else(|error| error.into_inner());
                for (mipmap, records) in state.channels.iter_mut().zip(completed) {
                    for record in records {
                        mipmap.push(record);
                    }
                }
                state.tails.fill(None);
                state.complete = true;
                state.generation = state.generation.wrapping_add(1);
                return Ok(());
            }
            Err(error) => {
                let message = error.to_string();
                let mut state = state.write().unwrap_or_else(|error| error.into_inner());
                state.error = Some(message);
                state.complete = true;
                state.generation = state.generation.wrapping_add(1);
                return Err(error);
            }
        }
    }
}

fn records_to_segments(
    records: &[WaveformRecord],
    start_sample: u64,
    end_sample: u64,
) -> Vec<CaptureWaveformSegment> {
    let mut segments = Vec::with_capacity(records.len());
    for record in records {
        let start = record.start_sample.max(start_sample);
        let end = record.end_sample.min(end_sample);
        if start >= end {
            continue;
        }
        let segment = if record.activity {
            CaptureWaveformSegment::Activity {
                start_sample: start,
                end_sample: end,
                first: record.first,
                last: record.last,
            }
        } else {
            CaptureWaveformSegment::Level {
                start_sample: start,
                end_sample: end,
                value: record.first,
            }
        };
        segments.push(segment);
    }
    segments
}

fn format_sample_rate(sample_rate_hz: f64) -> String {
    if sample_rate_hz >= 1_000_000_000.0 {
        format!("{:.3} GHz", sample_rate_hz / 1_000_000_000.0)
    } else if sample_rate_hz >= 1_000_000.0 {
        format!("{:.3} MHz", sample_rate_hz / 1_000_000.0)
    } else if sample_rate_hz >= 1_000.0 {
        format!("{:.3} kHz", sample_rate_hz / 1_000.0)
    } else {
        format!("{sample_rate_hz:.0} Hz")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tempfile::tempdir;

    use crate::{
        CaptureChannelId, CaptureChunk, CaptureChunkWriter, CaptureIndex, CaptureSessionId,
        CaptureStoreDescriptor, CaptureWaveformSegment, NativeCaptureStore,
        NativeCaptureStoreConfig,
    };

    use super::NativeGrowingCaptureIndex;

    fn level_at(sample: u64, channel: usize) -> bool {
        (sample / (37 + channel as u64 * 11) + channel as u64).is_multiple_of(2)
    }

    fn chunk(
        session: CaptureSessionId,
        channels: Arc<[CaptureChannelId]>,
        sequence: u64,
        start_sample: u64,
        sample_count: u64,
    ) -> CaptureChunk {
        let bit_offset = ((sequence * 5 + 3) % 8) as u8;
        let bit_count = sample_count as usize * channels.len();
        let mut bytes = vec![0_u8; (usize::from(bit_offset) + bit_count).div_ceil(8)];
        for relative in 0..sample_count {
            for channel in 0..channels.len() {
                if level_at(start_sample + relative, channel) {
                    let bit = usize::from(bit_offset) + relative as usize * channels.len() + channel;
                    bytes[bit / 8] |= 1 << (bit % 8);
                }
            }
        }
        CaptureChunk::packed_lsb_first(
            session,
            sequence,
            start_sample,
            sample_count,
            channels,
            bytes,
            bit_offset,
        )
        .unwrap()
    }

    fn wait_for_generation(index: &NativeGrowingCaptureIndex, generation: u64) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while index.generation() < generation {
            assert!(Instant::now() < deadline, "growing index timed out");
            std::thread::yield_now();
        }
    }

    fn expected_transitions(
        channel: usize,
        start_sample: u64,
        end_sample: u64,
    ) -> Vec<crate::CaptureTransition> {
        let mut previous = level_at(start_sample, channel);
        let mut transitions = Vec::new();
        for sample in start_sample + 1..end_sample {
            let value = level_at(sample, channel);
            if value != previous {
                transitions.push(crate::CaptureTransition { sample, value });
                previous = value;
            }
        }
        transitions
    }

    #[test]
    fn growing_query_is_visible_before_completion_and_matches_final_raw_and_summary_data() {
        let temporary = tempdir().unwrap();
        let session = CaptureSessionId::new(0x71a3);
        let channels: Arc<[CaptureChannelId]> = vec![
            CaptureChannelId::new("bank-a:7"),
            CaptureChannelId::new("bank-c:2"),
        ]
        .into();
        let descriptor = CaptureStoreDescriptor::new(session, Arc::clone(&channels)).unwrap();
        let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor)
            .with_commit_batch_chunks(1)
            .unwrap();
        let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
        let (mut index, worker) = NativeGrowingCaptureIndex::spawn(
            store.clone(),
            "Growing test",
            1_000_000.0,
            vec!["A7".into(), "C2".into()],
        )
        .unwrap();

        writer
            .append(chunk(session, Arc::clone(&channels), 0, 0, 75))
            .unwrap();
        wait_for_generation(&index, 1);
        assert!(!index.is_complete());
        assert_eq!(index.current_metadata().total_samples, 75);
        let live = index.sampled_window(&[0, 1], 0, 75, 75).unwrap();
        for channel in &live.channels {
            assert_eq!(channel.initial, level_at(0, channel.channel));
            assert_eq!(channel.transitions, expected_transitions(channel.channel, 0, 75));
        }

        writer
            .append(chunk(session, Arc::clone(&channels), 1, 75, 5_000))
            .unwrap();
        writer
            .append(chunk(session, Arc::clone(&channels), 2, 5_075, 6_000))
            .unwrap();
        writer.finish().unwrap();
        drop(writer);
        worker.join().unwrap();
        store.finalize().unwrap();

        let total_samples = 11_075;
        assert!(index.is_complete());
        assert_eq!(index.current_metadata().total_samples, total_samples);
        let exact = index
            .sampled_window(&[0, 1], 0, total_samples, total_samples as usize)
            .unwrap();
        for channel in &exact.channels {
            assert_eq!(channel.initial, level_at(0, channel.channel));
            assert_eq!(
                channel.transitions,
                expected_transitions(channel.channel, 0, total_samples)
            );
        }

        let summary = index.sampled_window(&[0, 1], 0, total_samples, 1).unwrap();
        assert!(summary.sample_step > 1);
        for channel in &summary.channels {
            let mut next = 0;
            for segment in &channel.waveform {
                let (start, end) = match *segment {
                    CaptureWaveformSegment::Level {
                        start_sample,
                        end_sample,
                        value,
                    } => {
                        assert!((start_sample..end_sample)
                            .all(|sample| level_at(sample, channel.channel) == value));
                        (start_sample, end_sample)
                    }
                    CaptureWaveformSegment::Activity {
                        start_sample,
                        end_sample,
                        first,
                        last,
                    } => {
                        assert_eq!(first, level_at(start_sample, channel.channel));
                        assert_eq!(last, level_at(end_sample - 1, channel.channel));
                        assert!((start_sample + 1..end_sample).any(|sample| {
                            level_at(sample - 1, channel.channel)
                                != level_at(sample, channel.channel)
                        }));
                        (start_sample, end_sample)
                    }
                    CaptureWaveformSegment::Edge { .. } => {
                        panic!("coarse growing summaries use level/activity segments")
                    }
                };
                assert_eq!(start, next);
                next = end;
            }
            assert_eq!(next, total_samples);
        }
    }
}
