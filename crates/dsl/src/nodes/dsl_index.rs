use super::capture::{
    BlockCaptureSource, CaptureActivity, CaptureBucket, CaptureMetadata, CaptureSampledChannel,
    CaptureSampledWindow, CaptureSource, CaptureSourceFactory, CaptureTransition, packed_bit,
};
use super::dsl_file::DslFileCaptureFactory;
use crate::{DslError, Result};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

const BLOCKS_PER_ROOT: usize = 1;
const L1_GROUP_SAMPLES: u64 = 64;
const L2_GROUP_SAMPLES: u64 = 4_096;
const L3_GROUP_SAMPLES: u64 = 262_144;
const L1_WORDS: usize = 4_096;
const L2_WORDS: usize = 64;
const MAGIC: &[u8; 8] = b"DSLIDX03";
const HEADER_SIZE: u64 = 96;
const DIR_ENTRY_SIZE: u64 = 48;

#[derive(Debug, Clone, Copy)]
struct IndexHeader {
    source_len: u64,
    total_samples: u64,
    total_blocks: u64,
    samples_per_block: u64,
    samplerate_bits: u64,
    total_channels: u32,
    roots_per_channel: u32,
    dir_offset: u64,
    payload_offset: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct RootDirEntry {
    first_block: u64,
    block_count: u32,
    offset: u64,
    len: u64,
}

#[derive(Debug, Clone)]
struct LeafSummary {
    valid_samples: u32,
    first: bool,
    last: bool,
    active: bool,
    l1_toggle: Vec<u64>,
    l1_last: Vec<u64>,
    l2_toggle: [u64; L2_WORDS],
    l2_last: [u64; L2_WORDS],
    l3_toggle: u64,
    l3_last: u64,
}

#[derive(Debug, Clone)]
struct RootChunk {
    channel: usize,
    root_index: usize,
    first_block: u64,
    block_count: u32,
    root_toggle: u64,
    root_first: u64,
    root_last: u64,
    leaves: Vec<LeafSummary>,
}

#[derive(Debug, Clone, Copy)]
struct GroupSummary {
    toggle: bool,
    last: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DslIndexProgress {
    pub completed_roots: usize,
    pub total_roots: usize,
}

impl DslIndexProgress {
    pub fn fraction(self) -> f32 {
        if self.total_roots == 0 {
            1.0
        } else {
            self.completed_roots as f32 / self.total_roots as f32
        }
    }
}

/// Chunked, sidecar-backed capture reader for interactive waveform viewing.
///
/// This reader builds/loads a `.dsl.idx` sidecar containing per-channel root chunks.
/// Each root chunk covers up to 64 raw DSL blocks and contains toggle + last-value
/// summaries. Zoomed-out windows are served from the sidecar index without reading
/// raw ZIP blocks. Deep zoom below 64 samples per display point falls back to the
/// existing raw reader, still on the worker thread.
pub struct IndexedCaptureReader<F: CaptureSourceFactory> {
    factory: F,
    index_path: PathBuf,
    header: CaptureMetadata,
    index_file: File,
    directory: Vec<Vec<RootDirEntry>>,
    root_cache: HashMap<(usize, usize), Arc<RootChunk>>,
    root_cache_order: VecDeque<(usize, usize)>,
    max_cached_roots: usize,
    raw_reader: F::Source,
}

pub type DslChunkedCaptureReader = IndexedCaptureReader<DslFileCaptureFactory>;

impl IndexedCaptureReader<DslFileCaptureFactory> {
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        let factory = DslFileCaptureFactory::open(path)?;
        Self::open_factory_with_progress(factory, |_| {})
    }

    pub fn open_with_progress<P, C>(path: P, progress: C) -> Result<Self>
    where
        P: AsRef<std::path::Path>,
        C: FnMut(DslIndexProgress),
    {
        let factory = DslFileCaptureFactory::open(path)?;
        Self::open_factory_with_progress(factory, progress)
    }
}

impl<F> IndexedCaptureReader<F>
where
    F: CaptureSourceFactory,
{
    const DEFAULT_MAX_CACHED_ROOTS: usize = 8;

    pub fn open_factory(factory: F) -> Result<Self> {
        Self::open_factory_with_progress(factory, |_| {})
    }

    pub fn open_factory_with_progress<C>(factory: F, progress: C) -> Result<Self>
    where
        C: FnMut(DslIndexProgress),
    {
        let header = factory.metadata().clone();
        let fingerprint = factory.fingerprint();
        let index_path = factory
            .index_path()
            .ok_or_else(|| DslError::ParseError("capture source is not indexable".to_string()))?;

        if !valid_index(&index_path, &header, fingerprint.revision)? {
            build_index(
                &factory,
                &index_path,
                &header,
                fingerprint.revision,
                progress,
            )?;
        }

        let mut index_file = File::open(&index_path)?;
        let index_header = read_index_header(&mut index_file)?;
        validate_index_header(&index_header, &header, fingerprint.revision)?;
        let roots_per_channel = index_header.roots_per_channel as usize;
        let directory = read_directory(
            &mut index_file,
            &index_header,
            header.total_probes,
            roots_per_channel,
        )?;
        let raw_reader = factory.open()?;

        Ok(Self {
            factory,
            index_path,
            header,
            index_file,
            directory,
            root_cache: HashMap::new(),
            root_cache_order: VecDeque::new(),
            max_cached_roots: Self::DEFAULT_MAX_CACHED_ROOTS,
            raw_reader,
        })
    }

    pub fn with_max_cached_roots(mut self, max_cached_roots: usize) -> Self {
        self.max_cached_roots = max_cached_roots.max(1);
        self.trim_root_cache();
        self
    }

    pub fn display_name(&self) -> String {
        self.factory.display_name()
    }

    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    pub fn header(&self) -> &CaptureMetadata {
        &self.header
    }

    pub fn capture_duration_us(&self) -> f64 {
        self.header.total_samples as f64 * 1_000_000.0 / self.header.samplerate_hz
    }

    pub fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> Result<CaptureSampledWindow> {
        let start_sample = start_sample.min(self.header.total_samples.saturating_sub(1));
        let end_sample = end_sample.clamp(start_sample + 1, self.header.total_samples);
        let samples = end_sample - start_sample;
        let target_points = target_points.max(1) as u64;
        let sample_step = samples.div_ceil(target_points).max(1);

        if sample_step < L1_GROUP_SAMPLES {
            return self.raw_reader.sampled_window(
                channels,
                start_sample,
                end_sample,
                target_points as usize,
            );
        }

        let group_samples = if sample_step >= self.header.samples_per_block {
            self.header.samples_per_block
        } else if sample_step >= L3_GROUP_SAMPLES {
            L3_GROUP_SAMPLES
        } else if sample_step >= L2_GROUP_SAMPLES {
            L2_GROUP_SAMPLES
        } else {
            L1_GROUP_SAMPLES
        };

        let mut sampled_channels = Vec::with_capacity(channels.len());
        for &channel in channels {
            sampled_channels.push(self.sample_indexed_channel(
                channel,
                start_sample,
                end_sample,
                target_points as usize,
                group_samples,
            )?);
        }

        Ok(CaptureSampledWindow {
            start_sample,
            end_sample,
            sample_step: group_samples,
            channels: sampled_channels,
        })
    }

    fn sample_indexed_channel(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
        group_samples: u64,
    ) -> Result<CaptureSampledChannel> {
        if channel >= self.header.total_probes {
            return Err(DslError::InvalidProbe(channel));
        }

        let name = self
            .header
            .probe_names
            .get(channel)
            .cloned()
            .unwrap_or_else(|| format!("Probe{}", channel));
        let initial = self.value_at_group_start(channel, start_sample, group_samples)?;
        let mut current = initial;
        let mut transitions = Vec::new();
        let mut activities = Vec::new();
        let mut buckets = Vec::new();

        let samples = end_sample - start_sample;
        let target_points = target_points.max(1) as u64;
        let mut previous_end = start_sample;

        for point in 0..target_points {
            let visible_start = start_sample + samples.saturating_mul(point) / target_points;
            let visible_end = if point + 1 == target_points {
                end_sample
            } else {
                start_sample + samples.saturating_mul(point + 1) / target_points
            };
            if visible_end <= visible_start || visible_start < previous_end {
                continue;
            }
            previous_end = visible_end;

            let summary = self.range_summary(channel, visible_start, visible_end, group_samples)?;
            buckets.push(CaptureBucket {
                start_sample: visible_start,
                end_sample: visible_end,
                toggle: summary.toggle,
                last: summary.last,
            });
            if summary.toggle {
                activities.push(CaptureActivity {
                    start_sample: visible_start,
                    end_sample: visible_end,
                });
                let visible_edge = visible_start + (visible_end - visible_start) / 2;
                if summary.last != current {
                    transitions.push(CaptureTransition {
                        sample: visible_edge,
                        value: summary.last,
                    });
                }
            }
            current = summary.last;
        }

        Ok(CaptureSampledChannel {
            channel,
            name,
            initial,
            transitions,
            activities,
            buckets,
        })
    }

    fn value_at_group_start(
        &mut self,
        channel: usize,
        sample: u64,
        group_samples: u64,
    ) -> Result<bool> {
        if sample == 0 {
            let block = self.block_for_sample(0);
            let root_index = (block as usize) / BLOCKS_PER_ROOT;
            let root = self.load_root(channel, root_index)?;
            let leaf_index = (block - root.first_block) as usize;
            return Ok(root.leaves.get(leaf_index).is_some_and(|leaf| leaf.first));
        }

        let aligned = (sample / group_samples) * group_samples;
        if aligned == 0 {
            let block = self.block_for_sample(0);
            let root_index = (block as usize) / BLOCKS_PER_ROOT;
            let root = self.load_root(channel, root_index)?;
            let leaf_index = (block - root.first_block) as usize;
            return Ok(root.leaves.get(leaf_index).is_some_and(|leaf| leaf.first));
        }
        self.group_summary(channel, aligned - group_samples, group_samples)
            .map(|g| g.last)
    }

    fn range_summary(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
        group_samples: u64,
    ) -> Result<GroupSummary> {
        let mut group_start = (start_sample / group_samples) * group_samples;
        let mut toggle = false;
        let mut last = self.value_at_group_start(channel, start_sample, group_samples)?;

        while group_start < end_sample {
            let group_end = group_start
                .saturating_add(group_samples)
                .min(self.header.total_samples);
            if group_end > start_sample {
                let summary = self.group_summary(channel, group_start, group_samples)?;
                toggle |= summary.toggle;
                last = summary.last;
            }
            if group_end <= group_start {
                break;
            }
            group_start = group_end;
        }

        Ok(GroupSummary { toggle, last })
    }

    fn group_summary(
        &mut self,
        channel: usize,
        sample: u64,
        group_samples: u64,
    ) -> Result<GroupSummary> {
        let sample = sample.min(self.header.total_samples.saturating_sub(1));
        let block_index = self.block_for_sample(sample);
        let local = sample - block_index * self.header.samples_per_block;
        let root_index = (block_index as usize) / BLOCKS_PER_ROOT;
        let root = self.load_root(channel, root_index)?;
        let leaf_index = (block_index - root.first_block) as usize;
        let Some(leaf) = root.leaves.get(leaf_index) else {
            return Ok(GroupSummary {
                toggle: false,
                last: false,
            });
        };

        if !leaf.active {
            return Ok(GroupSummary {
                toggle: false,
                last: leaf.first,
            });
        }

        let summary = if group_samples >= self.header.samples_per_block {
            GroupSummary {
                toggle: leaf.active,
                last: leaf.last,
            }
        } else {
            match group_samples {
                L3_GROUP_SAMPLES => {
                    let idx = (local / L3_GROUP_SAMPLES).min(63) as usize;
                    GroupSummary {
                        toggle: bit(leaf.l3_toggle, idx),
                        last: bit(leaf.l3_last, idx),
                    }
                }
                L2_GROUP_SAMPLES => {
                    let group = (local / L2_GROUP_SAMPLES).min(4095) as usize;
                    let word = group / 64;
                    let bit_idx = group % 64;
                    GroupSummary {
                        toggle: bit(leaf.l2_toggle[word], bit_idx),
                        last: bit(leaf.l2_last[word], bit_idx),
                    }
                }
                _ => {
                    let group = (local / L1_GROUP_SAMPLES).min(262_143) as usize;
                    let word = group / 64;
                    let bit_idx = group % 64;
                    GroupSummary {
                        toggle: leaf
                            .l1_toggle
                            .get(word)
                            .is_some_and(|word| bit(*word, bit_idx)),
                        last: leaf
                            .l1_last
                            .get(word)
                            .is_some_and(|word| bit(*word, bit_idx)),
                    }
                }
            }
        };

        Ok(summary)
    }

    fn load_root(&mut self, channel: usize, root_index: usize) -> Result<Arc<RootChunk>> {
        let key = (channel, root_index);
        if let Some(root) = self.root_cache.get(&key).cloned() {
            self.touch_root_cache_key(key);
            return Ok(root);
        }

        let entry = self
            .directory
            .get(channel)
            .and_then(|roots| roots.get(root_index))
            .copied()
            .ok_or_else(|| DslError::ParseError("root chunk index out of bounds".to_string()))?;
        self.index_file.seek(SeekFrom::Start(entry.offset))?;
        let mut data = vec![0_u8; entry.len as usize];
        self.index_file.read_exact(&mut data)?;
        let root = Arc::new(deserialize_root_chunk(&data)?);
        self.root_cache.insert(key, Arc::clone(&root));
        self.root_cache_order.push_back(key);
        self.trim_root_cache();
        Ok(root)
    }

    fn block_for_sample(&self, sample: u64) -> u64 {
        sample / self.header.samples_per_block
    }

    fn touch_root_cache_key(&mut self, key: (usize, usize)) {
        if self
            .root_cache_order
            .back()
            .is_some_and(|existing| *existing == key)
        {
            return;
        }
        self.root_cache_order.retain(|existing| *existing != key);
        self.root_cache_order.push_back(key);
    }

    fn trim_root_cache(&mut self) {
        while self.root_cache.len() > self.max_cached_roots {
            if let Some(key) = self.root_cache_order.pop_front() {
                self.root_cache.remove(&key);
            } else {
                break;
            }
        }
    }
}

fn valid_index(path: &Path, header: &CaptureMetadata, source_len: u64) -> Result<bool> {
    let Ok(mut file) = File::open(path) else {
        return Ok(false);
    };
    let Ok(index_header) = read_index_header(&mut file) else {
        return Ok(false);
    };
    Ok(validate_index_header(&index_header, header, source_len).is_ok())
}

fn validate_index_header(
    index_header: &IndexHeader,
    header: &CaptureMetadata,
    source_len: u64,
) -> Result<()> {
    if index_header.source_len != source_len
        || index_header.total_samples != header.total_samples
        || index_header.total_blocks != header.total_blocks
        || index_header.samples_per_block != header.samples_per_block
        || index_header.samplerate_bits != header.samplerate_hz.to_bits()
        || index_header.total_channels != header.total_probes as u32
        || index_header.roots_per_channel
            != (header.total_blocks as usize).div_ceil(BLOCKS_PER_ROOT) as u32
    {
        return Err(DslError::ParseError(
            "waveform sidecar is stale".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct BuildJob {
    channel: usize,
    root_index: usize,
    first_block: u64,
    block_count: u32,
}

fn build_index<C, P>(
    factory: &C,
    index_path: &Path,
    header: &CaptureMetadata,
    source_len: u64,
    mut progress: P,
) -> Result<()>
where
    C: CaptureSourceFactory,
    P: FnMut(DslIndexProgress),
{
    let temp_path = index_path.with_extension("idx.tmp");
    let roots_per_channel = (header.total_blocks as usize).div_ceil(BLOCKS_PER_ROOT);
    let root_count = header.total_probes * roots_per_channel;
    let dir_offset = HEADER_SIZE;
    let payload_offset = dir_offset + root_count as u64 * DIR_ENTRY_SIZE;
    let mut directory = vec![vec![RootDirEntry::default(); roots_per_channel]; header.total_probes];
    let mut output = File::create(&temp_path)?;

    output.write_all(&vec![0_u8; payload_offset as usize])?;
    output.seek(SeekFrom::Start(payload_offset))?;

    let mut jobs = VecDeque::with_capacity(root_count);
    for channel in 0..header.total_probes {
        for root_index in 0..roots_per_channel {
            let first_block = (root_index * BLOCKS_PER_ROOT) as u64;
            if first_block >= header.total_blocks {
                continue;
            }
            let block_count =
                (header.total_blocks - first_block).min(BLOCKS_PER_ROOT as u64) as u32;
            jobs.push_back(BuildJob {
                channel,
                root_index,
                first_block,
                block_count,
            });
        }
    }

    let total_jobs = jobs.len();
    progress(DslIndexProgress {
        completed_roots: 0,
        total_roots: total_jobs,
    });

    let mut roots = build_roots_parallel(factory.clone(), header, jobs, &mut progress)?;
    for channel_roots in roots.iter_mut().take(header.total_probes) {
        let mut previous_last = None;
        for root in channel_roots.iter_mut().flatten() {
            apply_boundary_transition(root, previous_last);
            previous_last = root.leaves.last().map(|leaf| leaf.last);
            let offset = output.stream_position()?;
            let payload = serialize_root_chunk(&root);
            output.write_all(&payload)?;
            directory[root.channel][root.root_index] = RootDirEntry {
                first_block: root.first_block,
                block_count: root.block_count,
                offset,
                len: payload.len() as u64,
            };
        }
    }

    let index_header = IndexHeader {
        source_len,
        total_samples: header.total_samples,
        total_blocks: header.total_blocks,
        samples_per_block: header.samples_per_block,
        samplerate_bits: header.samplerate_hz.to_bits(),
        total_channels: header.total_probes as u32,
        roots_per_channel: roots_per_channel as u32,
        dir_offset,
        payload_offset,
    };
    output.seek(SeekFrom::Start(0))?;
    write_index_header(&mut output, &index_header)?;
    output.seek(SeekFrom::Start(dir_offset))?;
    for channel_dir in &directory {
        for entry in channel_dir {
            write_dir_entry(&mut output, entry)?;
        }
    }
    output.sync_all()?;
    drop(output);
    fs::rename(temp_path, index_path)?;
    Ok(())
}

fn build_roots_parallel<F>(
    factory: F,
    header: &CaptureMetadata,
    jobs: VecDeque<BuildJob>,
    progress: &mut impl FnMut(DslIndexProgress),
) -> Result<Vec<Vec<Option<RootChunk>>>>
where
    F: CaptureSourceFactory,
{
    let total_jobs = jobs.len();
    let roots_per_channel = (header.total_blocks as usize).div_ceil(BLOCKS_PER_ROOT);
    let mut roots = vec![vec![None; roots_per_channel]; header.total_probes];
    if total_jobs == 0 {
        return Ok(roots);
    }

    let worker_count = thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .saturating_sub(2)
        .max(1)
        .min(4)
        .min(total_jobs)
        .max(1);
    let jobs = Arc::new(Mutex::new(jobs));
    let header = Arc::new(header.clone());
    let (result_tx, result_rx) = mpsc::channel();
    let mut workers = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let jobs = Arc::clone(&jobs);
        let header = Arc::clone(&header);
        let factory = factory.clone();
        let result_tx = result_tx.clone();
        workers.push(thread::spawn(move || {
            let worker_result = || -> Result<()> {
                let mut source = factory.open()?;
                loop {
                    let Some(job) = jobs.lock().unwrap().pop_front() else {
                        break;
                    };
                    let mut previous_last = None;
                    let root = build_root_chunk(
                        &mut source,
                        &header,
                        job.channel,
                        job.root_index,
                        job.first_block,
                        job.block_count,
                        &mut previous_last,
                    )?;
                    if result_tx.send(Ok((job, root))).is_err() {
                        break;
                    }
                }
                Ok(())
            }();

            if let Err(err) = worker_result {
                let _ = result_tx.send(Err(err));
            }
        }));
    }
    drop(result_tx);

    let mut received = 0;
    let mut first_error = None;
    while received < total_jobs {
        match result_rx.recv() {
            Ok(Ok((job, root))) => {
                roots[job.channel][job.root_index] = Some(root);
                received += 1;
                progress(DslIndexProgress {
                    completed_roots: received,
                    total_roots: total_jobs,
                });
            }
            Ok(Err(err)) => {
                first_error = Some(err);
                break;
            }
            Err(_) => break,
        }
    }

    for worker in workers {
        if worker.join().is_err() && first_error.is_none() {
            first_error = Some(DslError::ParseError(
                "waveform index worker panicked".to_string(),
            ));
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }
    if received != total_jobs {
        return Err(DslError::ParseError(
            "waveform index build did not complete".to_string(),
        ));
    }
    Ok(roots)
}

fn build_root_chunk<S>(
    source: &mut S,
    header: &CaptureMetadata,
    channel: usize,
    root_index: usize,
    first_block: u64,
    block_count: u32,
    previous_last: &mut Option<bool>,
) -> Result<RootChunk>
where
    S: BlockCaptureSource,
{
    let mut root_toggle = 0_u64;
    let mut root_first = 0_u64;
    let mut root_last = 0_u64;
    let mut leaves = Vec::with_capacity(block_count as usize);

    for leaf_index in 0..block_count as usize {
        let block = first_block + leaf_index as u64;
        let data = source.read_packed_block(channel, block)?;
        let block_start = block * header.samples_per_block;
        let remaining = header.total_samples.saturating_sub(block_start);
        let valid_samples = ((data.len() as u64) * 8).min(remaining);
        let leaf = build_leaf_summary(&data, valid_samples, *previous_last);
        if leaf.active {
            root_toggle |= 1_u64 << leaf_index;
        }
        if leaf.first {
            root_first |= 1_u64 << leaf_index;
        }
        if leaf.last {
            root_last |= 1_u64 << leaf_index;
        }
        *previous_last = Some(leaf.last);
        leaves.push(leaf);
    }

    Ok(RootChunk {
        channel,
        root_index,
        first_block,
        block_count,
        root_toggle,
        root_first,
        root_last,
        leaves,
    })
}

fn build_leaf_summary(data: &[u8], valid_samples: u64, previous_last: Option<bool>) -> LeafSummary {
    let valid_samples = valid_samples.min(u32::MAX as u64) as u32;
    if valid_samples == 0 {
        return LeafSummary {
            valid_samples,
            first: false,
            last: false,
            active: false,
            l1_toggle: Vec::new(),
            l1_last: Vec::new(),
            l2_toggle: [0; L2_WORDS],
            l2_last: [0; L2_WORDS],
            l3_toggle: 0,
            l3_last: 0,
        };
    }

    let first = packed_bit(data, 0);
    let last = packed_bit(data, valid_samples as usize - 1);
    let mut entering = previous_last.unwrap_or(first);
    let l1_groups = (valid_samples as usize).div_ceil(64);
    let mut l1_toggle = vec![0_u64; L1_WORDS];
    let mut l1_last = vec![0_u64; L1_WORDS];

    for group in 0..l1_groups {
        let start = group * 64;
        let end = ((group + 1) * 64).min(valid_samples as usize);
        let mut has_toggle = false;
        let mut prev = entering;
        for bit_index in start..end {
            let value = packed_bit(data, bit_index);
            if value != prev {
                has_toggle = true;
            }
            prev = value;
        }
        entering = prev;
        if has_toggle {
            set_bit(&mut l1_toggle[group / 64], group % 64);
        }
        if entering {
            set_bit(&mut l1_last[group / 64], group % 64);
        }
    }

    let mut l2_toggle = [0_u64; L2_WORDS];
    let mut l2_last = [0_u64; L2_WORDS];
    let l2_groups = l1_groups.div_ceil(64);
    for group in 0..l2_groups {
        let l1_word = l1_toggle[group];
        if l1_word != 0 {
            set_bit(&mut l2_toggle[group / 64], group % 64);
        }
        let last_l1_group = ((group + 1) * 64).min(l1_groups).saturating_sub(1);
        if bit(l1_last[last_l1_group / 64], last_l1_group % 64) {
            set_bit(&mut l2_last[group / 64], group % 64);
        }
    }

    let mut l3_toggle = 0_u64;
    let mut l3_last = 0_u64;
    let l3_groups = l2_groups.div_ceil(64);
    for group in 0..l3_groups {
        if l2_toggle[group] != 0 {
            set_bit(&mut l3_toggle, group);
        }
        let last_l2_group = ((group + 1) * 64).min(l2_groups).saturating_sub(1);
        if bit(l2_last[last_l2_group / 64], last_l2_group % 64) {
            set_bit(&mut l3_last, group);
        }
    }

    LeafSummary {
        valid_samples,
        first,
        last,
        active: l3_toggle != 0,
        l1_toggle: if l3_toggle != 0 {
            l1_toggle
        } else {
            Vec::new()
        },
        l1_last: if l3_toggle != 0 { l1_last } else { Vec::new() },
        l2_toggle,
        l2_last,
        l3_toggle,
        l3_last,
    }
}

fn apply_boundary_transition(root: &mut RootChunk, previous_last: Option<bool>) {
    let Some(leaf) = root.leaves.first_mut() else {
        return;
    };
    let Some(previous_last) = previous_last else {
        return;
    };
    if leaf.valid_samples == 0 || previous_last == leaf.first {
        return;
    }

    if !leaf.active {
        leaf.l1_toggle = vec![0_u64; L1_WORDS];
        leaf.l1_last = vec![0_u64; L1_WORDS];
        fill_constant_last_summaries(leaf);
        leaf.active = true;
    }

    set_bit(&mut leaf.l1_toggle[0], 0);
    set_bit(&mut leaf.l2_toggle[0], 0);
    set_bit(&mut leaf.l3_toggle, 0);
    set_bit(&mut root.root_toggle, 0);
}

fn fill_constant_last_summaries(leaf: &mut LeafSummary) {
    if !leaf.first || leaf.valid_samples == 0 {
        return;
    }

    let l1_groups = (leaf.valid_samples as usize).div_ceil(L1_GROUP_SAMPLES as usize);
    for group in 0..l1_groups {
        set_bit(&mut leaf.l1_last[group / 64], group % 64);
    }

    let l2_groups = l1_groups.div_ceil(64);
    for group in 0..l2_groups {
        set_bit(&mut leaf.l2_last[group / 64], group % 64);
    }

    let l3_groups = l2_groups.div_ceil(64);
    for group in 0..l3_groups {
        set_bit(&mut leaf.l3_last, group);
    }
}

fn serialize_root_chunk(root: &RootChunk) -> Vec<u8> {
    let mut out = Vec::new();
    push_u32(&mut out, root.channel as u32);
    push_u32(&mut out, root.root_index as u32);
    push_u64(&mut out, root.first_block);
    push_u32(&mut out, root.block_count);
    push_u32(&mut out, root.leaves.len() as u32);
    push_u64(&mut out, root.root_toggle);
    push_u64(&mut out, root.root_first);
    push_u64(&mut out, root.root_last);
    for leaf in &root.leaves {
        push_u32(&mut out, leaf.valid_samples);
        out.push((leaf.first as u8) | ((leaf.last as u8) << 1) | ((leaf.active as u8) << 2));
        out.extend_from_slice(&[0, 0, 0]);
        if leaf.active {
            push_u64_slice(&mut out, &leaf.l1_toggle);
            push_u64_slice(&mut out, &leaf.l1_last);
            push_u64_slice(&mut out, &leaf.l2_toggle);
            push_u64_slice(&mut out, &leaf.l2_last);
            push_u64(&mut out, leaf.l3_toggle);
            push_u64(&mut out, leaf.l3_last);
        }
    }
    out
}

fn deserialize_root_chunk(data: &[u8]) -> Result<RootChunk> {
    let mut cursor = Cursor::new(data);
    let channel = cursor.u32()? as usize;
    let root_index = cursor.u32()? as usize;
    let first_block = cursor.u64()?;
    let block_count = cursor.u32()?;
    let leaf_count = cursor.u32()? as usize;
    let root_toggle = cursor.u64()?;
    let root_first = cursor.u64()?;
    let root_last = cursor.u64()?;
    let mut leaves = Vec::with_capacity(leaf_count);
    for _ in 0..leaf_count {
        let valid_samples = cursor.u32()?;
        let flags = cursor.byte()?;
        cursor.skip(3)?;
        let active = flags & 0b100 != 0;
        let mut leaf = LeafSummary {
            valid_samples,
            first: flags & 0b001 != 0,
            last: flags & 0b010 != 0,
            active,
            l1_toggle: Vec::new(),
            l1_last: Vec::new(),
            l2_toggle: [0; L2_WORDS],
            l2_last: [0; L2_WORDS],
            l3_toggle: 0,
            l3_last: 0,
        };
        if active {
            leaf.l1_toggle = cursor.u64_vec(L1_WORDS)?;
            leaf.l1_last = cursor.u64_vec(L1_WORDS)?;
            leaf.l2_toggle.copy_from_slice(&cursor.u64_vec(L2_WORDS)?);
            leaf.l2_last.copy_from_slice(&cursor.u64_vec(L2_WORDS)?);
            leaf.l3_toggle = cursor.u64()?;
            leaf.l3_last = cursor.u64()?;
        }
        leaves.push(leaf);
    }
    Ok(RootChunk {
        channel,
        root_index,
        first_block,
        block_count,
        root_toggle,
        root_first,
        root_last,
        leaves,
    })
}

fn read_index_header(file: &mut File) -> Result<IndexHeader> {
    file.seek(SeekFrom::Start(0))?;
    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(DslError::ParseError(
            "invalid waveform sidecar magic".to_string(),
        ));
    }
    let version = read_u32(file)?;
    if version != 3 {
        return Err(DslError::ParseError(
            "unsupported waveform sidecar version".to_string(),
        ));
    }
    let _header_size = read_u32(file)?;
    let source_len = read_u64(file)?;
    let total_samples = read_u64(file)?;
    let total_blocks = read_u64(file)?;
    let samples_per_block = read_u64(file)?;
    let samplerate_bits = read_u64(file)?;
    let total_channels = read_u32(file)?;
    let roots_per_channel = read_u32(file)?;
    let dir_offset = read_u64(file)?;
    let payload_offset = read_u64(file)?;
    Ok(IndexHeader {
        source_len,
        total_samples,
        total_blocks,
        samples_per_block,
        samplerate_bits,
        total_channels,
        roots_per_channel,
        dir_offset,
        payload_offset,
    })
}

fn write_index_header(file: &mut File, header: &IndexHeader) -> Result<()> {
    file.write_all(MAGIC)?;
    write_u32(file, 3)?;
    write_u32(file, HEADER_SIZE as u32)?;
    write_u64(file, header.source_len)?;
    write_u64(file, header.total_samples)?;
    write_u64(file, header.total_blocks)?;
    write_u64(file, header.samples_per_block)?;
    write_u64(file, header.samplerate_bits)?;
    write_u32(file, header.total_channels)?;
    write_u32(file, header.roots_per_channel)?;
    write_u64(file, header.dir_offset)?;
    write_u64(file, header.payload_offset)?;
    let written = 8 + 4 + 4 + 8 * 7 + 4 * 2;
    file.write_all(&vec![0_u8; HEADER_SIZE as usize - written])?;
    Ok(())
}

fn read_directory(
    file: &mut File,
    header: &IndexHeader,
    channels: usize,
    roots_per_channel: usize,
) -> Result<Vec<Vec<RootDirEntry>>> {
    file.seek(SeekFrom::Start(header.dir_offset))?;
    let mut directory = vec![vec![RootDirEntry::default(); roots_per_channel]; channels];
    for channel_dir in &mut directory {
        for entry in channel_dir {
            *entry = read_dir_entry(file)?;
        }
    }
    Ok(directory)
}

fn read_dir_entry(file: &mut File) -> Result<RootDirEntry> {
    let first_block = read_u64(file)?;
    let block_count = read_u32(file)?;
    let _reserved = read_u32(file)?;
    let offset = read_u64(file)?;
    let len = read_u64(file)?;
    let _reserved0 = read_u64(file)?;
    let _reserved1 = read_u64(file)?;
    Ok(RootDirEntry {
        first_block,
        block_count,
        offset,
        len,
    })
}

fn write_dir_entry(file: &mut File, entry: &RootDirEntry) -> Result<()> {
    write_u64(file, entry.first_block)?;
    write_u32(file, entry.block_count)?;
    write_u32(file, 0)?;
    write_u64(file, entry.offset)?;
    write_u64(file, entry.len)?;
    write_u64(file, 0)?;
    write_u64(file, 0)?;
    Ok(())
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn byte(&mut self) -> Result<u8> {
        let byte = *self
            .data
            .get(self.pos)
            .ok_or_else(|| DslError::ParseError("truncated waveform sidecar".to_string()))?;
        self.pos += 1;
        Ok(byte)
    }

    fn skip(&mut self, count: usize) -> Result<()> {
        if self.pos + count > self.data.len() {
            return Err(DslError::ParseError(
                "truncated waveform sidecar".to_string(),
            ));
        }
        self.pos += count;
        Ok(())
    }

    fn u32(&mut self) -> Result<u32> {
        let mut buf = [0_u8; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn u64(&mut self) -> Result<u64> {
        let mut buf = [0_u8; 8];
        self.read_exact(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn u64_vec(&mut self, len: usize) -> Result<Vec<u64>> {
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(self.u64()?);
        }
        Ok(out)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        if self.pos + buf.len() > self.data.len() {
            return Err(DslError::ParseError(
                "truncated waveform sidecar".to_string(),
            ));
        }
        buf.copy_from_slice(&self.data[self.pos..self.pos + buf.len()]);
        self.pos += buf.len();
        Ok(())
    }
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64_slice(out: &mut Vec<u8>, values: &[u64]) {
    for value in values {
        push_u64(out, *value);
    }
}

fn read_u32(file: &mut File) -> Result<u32> {
    let mut buf = [0_u8; 4];
    file.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(file: &mut File) -> Result<u64> {
    let mut buf = [0_u8; 8];
    file.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn write_u32(file: &mut File, value: u32) -> Result<()> {
    file.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn write_u64(file: &mut File, value: u64) -> Result<()> {
    file.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn bit(word: u64, index: usize) -> bool {
    index < 64 && ((word >> index) & 1) != 0
}

fn set_bit(word: &mut u64, index: usize) {
    if index < 64 {
        *word |= 1_u64 << index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_leaf_stores_only_root_values() {
        let data = vec![0_u8; 128];
        let leaf = build_leaf_summary(&data, 1024, None);

        assert!(!leaf.first);
        assert!(!leaf.last);
        assert!(!leaf.active);
        assert!(leaf.l1_toggle.is_empty());
        assert!(leaf.l1_last.is_empty());
        assert_eq!(leaf.l3_toggle, 0);
    }

    #[test]
    fn boundary_toggle_activates_constant_leaf() {
        let data = vec![0xff_u8; 128];
        let leaf = build_leaf_summary(&data, 1024, Some(false));

        assert!(leaf.first);
        assert!(leaf.last);
        assert!(leaf.active);
        assert!(bit(leaf.l1_toggle[0], 0));
        assert!(bit(leaf.l1_last[0], 0));
        assert!(bit(leaf.l2_toggle[0], 0));
        assert!(bit(leaf.l2_last[0], 0));
        assert!(bit(leaf.l3_toggle, 0));
        assert!(bit(leaf.l3_last, 0));
    }

    #[test]
    fn last_value_tracks_group_exit_level() {
        let mut data = vec![0_u8; 16];
        for byte in &mut data[8..16] {
            *byte = 0xff;
        }
        let leaf = build_leaf_summary(&data, 128, Some(false));

        assert!(!bit(leaf.l1_toggle[0], 0));
        assert!(!bit(leaf.l1_last[0], 0));
        assert!(bit(leaf.l1_toggle[0], 1));
        assert!(bit(leaf.l1_last[0], 1));
        assert!(bit(leaf.l2_toggle[0], 0));
        assert!(bit(leaf.l2_last[0], 0));
        assert!(bit(leaf.l3_toggle, 0));
        assert!(bit(leaf.l3_last, 0));
    }

    #[test]
    fn root_chunk_round_trips_active_and_constant_leaves() {
        let active = build_leaf_summary(&[0_u8, 0xff], 16, Some(false));
        let constant = build_leaf_summary(&[0xff_u8; 8], 64, Some(true));
        let root = RootChunk {
            channel: 2,
            root_index: 3,
            first_block: 192,
            block_count: 2,
            root_toggle: 0b01,
            root_first: 0b10,
            root_last: 0b11,
            leaves: vec![active, constant],
        };

        let data = serialize_root_chunk(&root);
        let decoded = deserialize_root_chunk(&data).expect("root chunk should decode");

        assert_eq!(decoded.channel, 2);
        assert_eq!(decoded.root_index, 3);
        assert_eq!(decoded.first_block, 192);
        assert_eq!(decoded.leaves.len(), 2);
        assert!(decoded.leaves[0].active);
        assert!(!decoded.leaves[1].active);
        assert!(bit(decoded.leaves[0].l1_toggle[0], 0));
    }

    #[test]
    fn chunked_reader_builds_sidecar_and_samples_window() -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};
        use zip::write::SimpleFileOptions;
        use zip::{CompressionMethod, ZipWriter};

        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("dsl-index-test-{id}.dsl"));
        let file = File::create(&path)?;
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        zip.start_file("header", options)?;
        zip.write_all(
            b"[version]\nversion = 3\n[header]\ntotal samples = 128\ntotal probes = 1\ntotal blocks = 1\nsamplerate = 1 MHz\nprobe0 = 0\n",
        )?;
        zip.start_file("L-0/0", options)?;
        let mut samples = [0_u8; 16];
        samples[8..16].fill(0xff);
        zip.write_all(&samples)?;
        zip.finish()?;

        let mut reader = DslChunkedCaptureReader::open(&path)?;
        assert!(reader.index_path().exists());
        let window = reader.sampled_window(&[0], 0, 128, 2)?;
        assert_eq!(window.channels.len(), 1);
        assert!(!window.channels[0].initial);
        assert_eq!(window.channels[0].activities.len(), 1);
        assert_eq!(window.channels[0].activities[0].start_sample, 64);
        assert_eq!(window.channels[0].activities[0].end_sample, 128);
        assert_eq!(window.channels[0].transitions.len(), 1);
        assert_eq!(window.channels[0].transitions[0].sample, 96);
        assert!(window.channels[0].transitions[0].value);

        let _ = fs::remove_file(reader.index_path());
        let _ = fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn full_file_sampling_uses_block_level_when_zoomed_out() -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};
        use zip::write::SimpleFileOptions;
        use zip::{CompressionMethod, ZipWriter};

        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("dsl-index-zoomed-out-{id}.dsl"));
        let file = File::create(&path)?;
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        zip.start_file("header", options)?;
        zip.write_all(
            b"[version]\nversion = 3\n[header]\ntotal samples = 33554432\ntotal probes = 1\ntotal blocks = 2\nsamplerate = 1 MHz\nprobe0 = 0\n",
        )?;
        zip.start_file("L-0/0", options)?;
        zip.write_all(&vec![0_u8; 2 * 1024 * 1024])?;
        zip.start_file("L-0/1", options)?;
        zip.write_all(&vec![0xff_u8; 2 * 1024 * 1024])?;
        zip.finish()?;

        let mut reader = DslChunkedCaptureReader::open(&path)?;
        let window = reader.sampled_window(&[0], 0, 33_554_432, 2)?;

        assert_eq!(window.sample_step, 16_777_216);
        assert_eq!(window.channels[0].buckets.len(), 2);
        assert_eq!(window.channels[0].buckets[0].start_sample, 0);
        assert_eq!(window.channels[0].buckets[0].end_sample, 16_777_216);
        assert!(!window.channels[0].buckets[0].toggle);
        assert!(!window.channels[0].buckets[0].last);
        assert_eq!(window.channels[0].buckets[1].start_sample, 16_777_216);
        assert_eq!(window.channels[0].buckets[1].end_sample, 33_554_432);
        assert!(window.channels[0].buckets[1].toggle);
        assert!(window.channels[0].buckets[1].last);
        assert_eq!(window.channels[0].transitions.len(), 1);
        assert!(window.channels[0].transitions[0].sample >= 16_777_216);

        let _ = fs::remove_file(reader.index_path());
        let _ = fs::remove_file(path);
        Ok(())
    }
}
