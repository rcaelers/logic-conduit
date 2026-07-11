//! DSL file source
//!
//! Provides `DslFileSource` - a runtime process node that reads DSLogic .dsl capture files
//! and outputs Sample streams per channel (run-length encoded for efficiency).
//!
//! Each broadcast destination runs in its own independent reading thread, so a slow consumer
//! on one destination never blocks other destinations. All threads share a single ZipArchive
//! and block cache via `Arc<Mutex<..>>`.

use crate::runtime::events::TextSample;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkResult};
use crate::runtime::sample::{Sample, SampleBlock};
use crate::runtime::{
    BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureSource,
    CaptureTransition, DslHeader, DslSampledWindow, EdgeQuery, ProtocolKind, SampleKind, Sender,
};
use crate::runtime::{CaptureIndexProgress, IndexSampler};
use crate::{Error, Result};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tracing::{debug, info, warn};
use zip::ZipArchive;

type BlockCache = Arc<Mutex<HashMap<(usize, u64), Arc<[u8]>>>>;

/// Windowed DSLogic capture reader for interactive viewers.
///
/// Unlike [`DslFileSource`], this reader is not a streaming graph source. It is
/// optimized for repeated random-access viewport reads and keeps only a bounded
/// number of packed-bit ZIP blocks in memory.
pub struct DslCaptureReader {
    path: PathBuf,
    archive: ZipArchive<File>,
    header: DslHeader,
    cache: HashMap<(usize, u64), Arc<[u8]>>,
    cache_order: VecDeque<(usize, u64)>,
    max_cached_blocks: usize,
}

impl DslCaptureReader {
    /// A single slot: enough to make sequential `read_sample` access viable
    /// (the current block stays decompressed). Block-level consumers get
    /// their caching from the mmap'd raw sidecar instead, so a larger LRU
    /// here would only duplicate it — notably during the index build, where
    /// every parallel worker holds its own reader. Callers that genuinely
    /// stream samples across blocks can raise it via
    /// [`DslCaptureReader::with_max_cached_blocks`].
    const DEFAULT_MAX_CACHED_BLOCKS: usize = 1;

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let mut archive = ZipArchive::new(file)?;
        let header = DslFileSource::parse_header(&mut archive)?;

        Ok(Self {
            path,
            archive,
            header,
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            max_cached_blocks: Self::DEFAULT_MAX_CACHED_BLOCKS,
        })
    }

    pub fn with_max_cached_blocks(mut self, max_cached_blocks: usize) -> Self {
        self.max_cached_blocks = max_cached_blocks.max(1);
        self.trim_cache();
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn header(&self) -> &DslHeader {
        &self.header
    }

    pub fn capture_duration_us(&self) -> f64 {
        self.header.duration_us()
    }

    pub fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> Result<DslSampledWindow> {
        CaptureSource::sampled_window(self, channels, start_sample, end_sample, target_points)
    }

    fn read_bit_cached(&mut self, channel: usize, position: u64) -> Result<bool> {
        if position >= self.header.total_samples {
            return Err(Error::OutOfBounds(position));
        }

        let block_num = position / self.header.samples_per_block;
        if block_num >= self.header.total_blocks {
            return Err(Error::OutOfBounds(position));
        }

        let sample_in_block = (position % self.header.samples_per_block) as usize;
        let key = (channel, block_num);
        let data = self.read_block_cached(key)?;
        Ok(DslFileSource::get_bit(&data, sample_in_block))
    }

    fn read_block_cached(&mut self, key: (usize, u64)) -> Result<Arc<[u8]>> {
        if let Some(data) = self.cache.get(&key).cloned() {
            self.touch_cache_key(key);
            return Ok(data);
        }

        let (channel, block_num) = key;
        let block_name = format!("L-{}/{}", channel, block_num);
        let data = {
            let mut file = self
                .archive
                .by_name(&block_name)
                .map_err(|_| Error::InvalidBlock(block_num))?;
            let mut data = Vec::new();
            file.read_to_end(&mut data)?;
            Arc::<[u8]>::from(data)
        };

        self.cache.insert(key, Arc::clone(&data));
        self.cache_order.push_back(key);
        self.trim_cache();
        Ok(data)
    }

    fn touch_cache_key(&mut self, key: (usize, u64)) {
        if self
            .cache_order
            .back()
            .is_some_and(|existing| *existing == key)
        {
            return;
        }
        self.cache_order.retain(|existing| *existing != key);
        self.cache_order.push_back(key);
    }

    fn trim_cache(&mut self) {
        while self.cache.len() > self.max_cached_blocks {
            if let Some(key) = self.cache_order.pop_front() {
                self.cache.remove(&key);
            } else {
                break;
            }
        }
    }
}

impl CaptureSource for DslCaptureReader {
    fn metadata(&self) -> &DslHeader {
        &self.header
    }

    fn read_sample(&mut self, channel: usize, position: u64) -> Result<bool> {
        self.read_bit_cached(channel, position)
    }
}

impl BlockCaptureSource for DslCaptureReader {
    fn read_packed_block(&mut self, channel: usize, block: u64) -> Result<BlockData> {
        self.read_block_cached((channel, block))
            .map(BlockData::from)
    }
}

#[derive(Debug, Clone)]
pub struct DslFileCaptureDataSource {
    path: PathBuf,
    header: DslHeader,
    source_len: u64,
    index_path: PathBuf,
}

impl DslFileCaptureDataSource {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let source_len = fs::metadata(&path)?.len();
        let file = File::open(&path)?;
        let mut archive = ZipArchive::new(file)?;
        let header = DslFileSource::parse_header(&mut archive)?;
        let index_path = dsl_sidecar_path(&path);

        Ok(Self {
            path,
            header,
            source_len,
            index_path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl CaptureDataSource for DslFileCaptureDataSource {
    type Reader = DslCaptureReader;

    fn open_reader(&self) -> Result<Self::Reader> {
        DslCaptureReader::open(&self.path)
    }

    fn metadata(&self) -> &DslHeader {
        &self.header
    }

    fn fingerprint(&self) -> CaptureFingerprint {
        CaptureFingerprint {
            revision: self.source_len,
        }
    }

    fn index_path(&self) -> Option<PathBuf> {
        Some(self.index_path.clone())
    }

    fn display_name(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("capture")
            .to_string()
    }
}

pub type DslChunkedCaptureReader = IndexSampler<DslCaptureReader>;

impl IndexSampler<DslCaptureReader> {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let source = DslFileCaptureDataSource::open(path)?;
        Self::open_data_source_with_progress(source, |_| {})
    }

    pub fn open_with_progress<P, C>(path: P, progress: C) -> Result<Self>
    where
        P: AsRef<Path>,
        C: FnMut(CaptureIndexProgress),
    {
        let source = DslFileCaptureDataSource::open(path)?;
        Self::open_data_source_with_progress(source, progress)
    }
}

fn dsl_sidecar_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("capture.dsl")
        .to_string();
    name.push_str(".idx");
    path.with_file_name(name)
}

/// File-backed [`EdgeQuery`] for one channel, sharing the same on-disk
/// `.idx`/`.raw` waveform index the viewer uses for random-access reads
/// (via [`DslChunkedCaptureReader`]).
struct DslChannelEdgeIndex {
    sampler: Arc<Mutex<DslChunkedCaptureReader>>,
    channel: usize,
    sample_period: f64,
    samplerate_hz: f64,
    total_samples: u64,
}

impl EdgeQuery for DslChannelEdgeIndex {
    fn sample_period(&self) -> f64 {
        self.sample_period
    }

    fn samplerate_hz(&self) -> f64 {
        self.samplerate_hz
    }

    fn total_samples(&self) -> u64 {
        self.total_samples
    }

    fn value_at(&self, position: u64) -> Result<bool> {
        let mut sampler = self.sampler.lock().unwrap();
        sampler.value_at(self.channel, position)
    }

    fn next_edge(&self, position: u64, limit: u64) -> Result<Option<CaptureTransition>> {
        let mut sampler = self.sampler.lock().unwrap();
        sampler.next_transition(self.channel, position, limit.min(self.total_samples))
    }

    fn next_edges(
        &self,
        position: u64,
        limit: u64,
        max_edges: usize,
        output: &mut Vec<CaptureTransition>,
    ) -> Result<()> {
        let mut sampler = self.sampler.lock().unwrap();
        sampler.next_transitions(
            self.channel,
            position,
            limit.min(self.total_samples),
            max_edges,
            output,
        )
    }

    fn values_at(&self, positions: &[u64], output: &mut Vec<bool>) -> Result<()> {
        let mut sampler = self.sampler.lock().unwrap();
        sampler.values_at(self.channel, positions, output)
    }
}

/// Source node that reads from a DSLogic .dsl capture file and outputs Sample streams
///
/// This runtime `ProcessNode` (with 0 inputs, N outputs) reads from a .dsl file and outputs
/// Sample streams for each channel (run-length encoded for efficiency).
///
/// ## Threading Model
///
/// This is a **self-threading node** (`is_self_threading() = true`). On the first (and only)
/// call to `work()`, it spawns one internal worker thread **per broadcast destination**.
/// The scheduler thread then waits for `should_stop()` to signal completion, rather than
/// calling `work()` repeatedly.
///
/// If a channel is broadcast to multiple receivers, each receiver gets its own independent
/// reading thread. This eliminates head-of-line blocking: slow consumers don't block fast ones.
/// All threads share a single ZipArchive and block cache via `Arc<Mutex<..>>`.
///
/// Example: If channel 0 connects to both `spi_decoder` and `parallel_decoder`, two threads
/// are spawned:
/// - Thread 1: reads channel 0 data → sends to `spi_decoder`
/// - Thread 2: reads channel 0 data → sends to `parallel_decoder`
///
/// If `parallel_decoder` blocks (waiting for enable signal), Thread 2 blocks but Thread 1
/// continues, ensuring `spi_decoder` receives data without interruption.
///
/// # Features
/// - Opens and parses DSLogic capture files (.dsl format)
/// - Per-destination threading eliminates head-of-line blocking
/// - On-demand block loading with shared caching for efficiency
/// - Automatic timestamp generation based on sample rate
/// - Sample output (only sends on signal transitions)
/// - Supports 1-16 channels
///
/// # Example
/// ```ignore
/// let source = DslFileSource::new("capture.dsl", 16)?;
/// let handle = pipeline.add_process(source);
/// ```
pub struct DslFileSource {
    name: String,
    // File access (shared across all channel threads)
    path: PathBuf,
    archive: Arc<Mutex<ZipArchive<File>>>,
    header: DslHeader,
    blocks: BlockCache,

    // Configuration
    num_channels: u8,
    max_samples: Option<u64>,

    // Per-channel thread management
    shutdown: Arc<AtomicBool>,
    threads_completed: Arc<AtomicUsize>,
    thread_handles: Option<Vec<JoinHandle<()>>>,
    threads_spawned: bool,
    num_threads: usize,

    // Lazily-built random-access waveform index, shared across every
    // channel's `edge_query()` handle. Built at most once, only if a
    // downstream node actually negotiates the `EdgeQuery` protocol — see
    // `edge_index_handle`.
    index: Mutex<Option<Arc<Mutex<DslChunkedCaptureReader>>>>,
}

impl DslFileSource {
    /// Create a new DSL file source from a file path
    pub fn new<P: AsRef<Path>>(path: P, num_channels: u8) -> Result<Self> {
        if !(1..=16).contains(&num_channels) {
            return Err(Error::ParseError(format!(
                "num_channels must be 1-16, got {}",
                num_channels
            )));
        }

        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let mut archive = ZipArchive::new(file)?;
        let header = Self::parse_header(&mut archive)?;

        if header.total_probes < num_channels as usize {
            return Err(Error::ParseError(format!(
                "File has only {} channels, need at least {}",
                header.total_probes, num_channels
            )));
        }

        Ok(Self {
            name: "dsl_file_source".to_string(),
            path,
            archive: Arc::new(Mutex::new(archive)),
            header: header.clone(),
            blocks: Arc::new(Mutex::new(HashMap::new())),
            num_channels,
            max_samples: None,
            shutdown: Arc::new(AtomicBool::new(false)),
            threads_completed: Arc::new(AtomicUsize::new(0)),
            thread_handles: None,
            threads_spawned: false,
            num_threads: 0,
            index: Mutex::new(None),
        })
    }

    /// Random-access handle backing `edge_query()`, built on first use from
    /// the same `.idx`/`.raw` sidecar files the viewer uses (via
    /// `DslFileCaptureDataSource`/`IndexSampler`). Returns `None` (logging a
    /// warning) if the index can't be built — callers fall back to `Stream`.
    fn edge_index_handle(&self) -> Option<Arc<Mutex<DslChunkedCaptureReader>>> {
        let mut guard = self.index.lock().unwrap();
        if guard.is_none() {
            let source = match DslFileCaptureDataSource::open(&self.path) {
                Ok(source) => source,
                Err(e) => {
                    warn!("Failed to open capture for edge queries: {}", e);
                    return None;
                }
            };
            match IndexSampler::open_data_source_with_progress(source, |_| {}) {
                Ok(sampler) => *guard = Some(Arc::new(Mutex::new(sampler))),
                Err(e) => {
                    warn!("Failed to build waveform index for edge queries: {}", e);
                    return None;
                }
            }
        }
        guard.clone()
    }

    pub(crate) fn parse_header(archive: &mut ZipArchive<File>) -> Result<DslHeader> {
        let mut header_file = archive
            .by_name("header")
            .map_err(|e| Error::ParseHeader(format!("Cannot find header file: {}", e)))?;

        let mut header_content = String::new();
        header_file.read_to_string(&mut header_content)?;
        drop(header_file); // Explicitly drop to release archive borrow

        let mut total_probes: Option<usize> = None;
        let mut samplerate: Option<String> = None;
        let mut total_samples: Option<u64> = None;
        let mut total_blocks: Option<u64> = None;
        let mut probe_names_map: HashMap<usize, String> = HashMap::new();

        for line in header_content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some(value) = line.strip_prefix("total probes = ") {
                total_probes = value.parse().ok();
            } else if let Some(value) = line.strip_prefix("samplerate = ") {
                samplerate = Some(value.to_string());
            } else if let Some(value) = line.strip_prefix("total samples = ") {
                total_samples = value.parse().ok();
            } else if let Some(value) = line.strip_prefix("total blocks = ") {
                total_blocks = value.parse().ok();
            } else if line.starts_with("probe")
                && let Some((probe_part, name)) = line.split_once(" = ")
                && let Some(num_str) = probe_part.strip_prefix("probe")
                && let Ok(probe_num) = num_str.parse::<usize>()
            {
                probe_names_map.insert(probe_num, name.to_string());
            }
        }

        let total_probes =
            total_probes.ok_or_else(|| Error::MissingField("total probes".to_string()))?;
        let samplerate = samplerate.ok_or_else(|| Error::MissingField("samplerate".to_string()))?;
        let total_samples =
            total_samples.ok_or_else(|| Error::MissingField("total samples".to_string()))?;
        let total_blocks =
            total_blocks.ok_or_else(|| Error::MissingField("total blocks".to_string()))?;

        let samplerate_hz = Self::parse_sample_rate(&samplerate)
            .ok_or_else(|| Error::ParseHeader(format!("Invalid sample rate: {}", samplerate)))?;
        let sample_period = 1.0 / samplerate_hz;

        // Determine actual block size by reading the first block (blocks are fixed-size except last)
        let samples_per_block = {
            let block_name = "L-0/0";
            let mut file = archive
                .by_name(block_name)
                .map_err(|_| Error::ParseHeader("Could not read first block".to_string()))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .map_err(|_| Error::ParseHeader("Could not read first block data".to_string()))?;
            (buf.len() * 8) as u64 // Convert bytes to bits/samples
        };

        debug!(
            "File has {} samples across {} blocks ({} samples/block standard size)",
            total_samples, total_blocks, samples_per_block
        );

        let probe_names = (0..total_probes)
            .map(|i| {
                probe_names_map
                    .get(&i)
                    .cloned()
                    .unwrap_or_else(|| format!("Probe{}", i))
            })
            .collect();

        Ok(DslHeader {
            total_probes,
            samplerate,
            samplerate_hz,
            sample_period,
            total_samples, // Use actual value from header file
            total_blocks,
            samples_per_block,
            probe_names,
        })
    }

    /// Get the header information
    pub fn header(&self) -> &DslHeader {
        &self.header
    }

    /// Get the total number of probes
    pub fn total_probes(&self) -> usize {
        self.header.total_probes
    }

    /// Get the total number of samples
    pub fn total_samples(&self) -> u64 {
        self.header.total_samples
    }

    /// Get the sample rate in Hz
    pub fn samplerate_hz(&self) -> f64 {
        self.header.samplerate_hz
    }

    /// Get the sample period in seconds
    pub fn sample_period(&self) -> f64 {
        self.header.sample_period
    }

    /// Get the total capture duration in seconds
    pub fn capture_duration(&self) -> f64 {
        self.header.total_samples as f64 * self.header.sample_period
    }

    /// Read a single bit from a specific channel at a specific position
    pub fn read_bit(&self, channel: usize, position: u64) -> Result<bool> {
        if channel >= self.header.total_probes {
            return Err(Error::InvalidProbe(channel));
        }
        if position >= self.header.total_samples {
            return Err(Error::OutOfBounds(position));
        }

        let block_num = position / self.header.samples_per_block;

        // Additional safety check: ensure block number is valid
        if block_num >= self.header.total_blocks {
            return Err(Error::OutOfBounds(position));
        }

        let sample_in_block = (position % self.header.samples_per_block) as usize;

        // Check cache first
        let key = (channel, block_num);
        {
            let blocks_guard = self.blocks.lock().unwrap();
            if let Some(data) = blocks_guard.get(&key) {
                return Ok(Self::get_bit(data, sample_in_block));
            }
        }

        // Load block
        let block_name = format!("L-{}/{}", channel, block_num);
        let data = {
            let mut archive_guard = self.archive.lock().unwrap();
            let mut file = archive_guard
                .by_name(&block_name)
                .map_err(|_| Error::InvalidBlock(block_num))?;

            let mut data = Vec::new();
            file.read_to_end(&mut data)?;
            Arc::<[u8]>::from(data)
        };

        let result = Self::get_bit(&data, sample_in_block);

        // Cache it
        let mut blocks_guard = self.blocks.lock().unwrap();
        blocks_guard.insert(key, data);

        Ok(result)
    }

    /// Set custom name (builder pattern)
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set maximum number of samples to read from file (for benchmarking)
    pub fn with_max_samples(mut self, max_samples: Option<u64>) -> Self {
        self.max_samples = max_samples;
        self
    }

    /// Get the number of channels this source outputs
    pub fn num_channels(&self) -> u8 {
        self.num_channels
    }

    // ── Associated Functions (Helpers) ──────────────────────────────────

    /// Extract a single bit from a byte array at the given bit index
    #[inline]
    pub(crate) fn get_bit(data: &[u8], bit_index: usize) -> bool {
        let byte_index = bit_index / 8;
        let bit_offset = bit_index % 8;

        if byte_index < data.len() {
            (data[byte_index] >> bit_offset) & 1 == 1
        } else {
            false
        }
    }

    /// Parse a sample rate string (e.g., "50 MHz") into Hz
    fn parse_sample_rate(samplerate: &str) -> Option<f64> {
        let parts: Vec<&str> = samplerate.split_whitespace().collect();
        if parts.len() >= 2
            && let Ok(value) = parts[0].parse::<f64>()
        {
            let multiplier = match parts[1] {
                "GHz" => 1_000_000_000.0,
                "MHz" => 1_000_000.0,
                "KHz" | "kHz" => 1_000.0,
                "Hz" => 1.0,
                _ => return None,
            };
            return Some(value * multiplier);
        }
        None
    }

    /// Worker thread that reads one channel's data and sends to one destination.
    ///
    /// Each thread loads blocks from the shared ZipArchive + cache, walks bits
    /// to detect edges, and sends Samples to its destination. Threads are
    /// fully independent — if a channel is broadcast to multiple destinations,
    /// each destination gets its own thread reading the same channel data.
    /// This eliminates head-of-line blocking: slow destinations don't block fast ones.
    ///
    /// Cross-channel temporal alignment is handled by downstream decoders
    /// using timestamps (e.g., `drain_before()` and `value_at_time()`).
    fn channel_reader_thread(config: ChannelReaderConfig) {
        let ChannelReaderConfig {
            archive,
            blocks,
            channel,
            header,
            sender,
            destination,
            max_samples,
            shutdown,
            completed,
        } = config;
        let label = channel_log_label(channel, destination.as_deref());
        let timestamp_step = (1_000_000_000.0 / header.samplerate_hz) as u64;
        let total_samples = max_samples
            .unwrap_or(header.total_samples)
            .min(header.total_samples);

        let mut current_value = false;
        let mut value_start_time: u64 = 0;
        let mut position: u64 = 0;
        let mut items_sent: u64 = 0;

        info!(
            "[{}] Starting channel reader thread ({} samples, {} blocks)",
            label, total_samples, header.total_blocks
        );

        for block_num in 0..header.total_blocks {
            if shutdown.load(Ordering::Relaxed) {
                debug!(
                    "[{}] Shutdown signal received at block {}",
                    label, block_num
                );
                break;
            }

            // Check if we've exceeded our sample limit
            let block_start_position = block_num * header.samples_per_block;
            if block_start_position >= total_samples {
                break;
            }

            // Load block data (check cache first, then archive)
            let block_data = {
                let key = (channel, block_num);

                // Check cache
                {
                    let cache_guard = blocks.lock().unwrap();
                    if let Some(data) = cache_guard.get(&key) {
                        Arc::clone(data)
                    } else {
                        drop(cache_guard);

                        // Load from archive
                        let block_name = format!("L-{}/{}", channel, block_num);
                        let data = {
                            let mut archive_guard = archive.lock().unwrap();
                            let mut file = match archive_guard.by_name(&block_name) {
                                Ok(f) => f,
                                Err(_) => {
                                    debug!("[{}] Block {} not found, stopping", label, block_num);
                                    break;
                                }
                            };
                            let mut buf = Vec::new();
                            if file.read_to_end(&mut buf).is_err() {
                                debug!("[{}] Failed to read block {}", label, block_num);
                                break;
                            }
                            Arc::<[u8]>::from(buf)
                        };

                        // Insert into cache
                        let mut cache_guard = blocks.lock().unwrap();
                        cache_guard.insert(key, Arc::clone(&data));
                        data
                    }
                }
            };

            // Walk bits in this block, detecting edges
            let block_capacity = (block_data.len() * 8) as u64;
            let samples_in_block = block_capacity.min(total_samples - block_start_position);

            for sample_in_block in 0..samples_in_block as usize {
                let value = Self::get_bit(&block_data, sample_in_block);
                let timestamp = position * timestamp_step;

                if position == 0 {
                    current_value = value;
                    value_start_time = timestamp;
                } else if value != current_value {
                    let edge = Sample::new(current_value, value_start_time);
                    if sender.send(edge).is_err() {
                        debug!(
                            "[{}] All receivers disconnected at position {}",
                            label, position
                        );
                        completed.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    items_sent += 1;

                    current_value = value;
                    value_start_time = timestamp;
                }

                position += 1;
            }

            if block_num > 0 && block_num % 10 == 0 {
                let pct = (position as f64 / total_samples as f64) * 100.0;
                debug!(
                    "[{}] Progress: {:.1}% ({} samples, {} edges sent)",
                    label, pct, position, items_sent
                );
            }
        }

        // Send final edge for the last value
        if position > 0 {
            let final_edge = Sample::new(current_value, value_start_time);
            let _ = sender.send(final_edge);
            items_sent += 1;
        }

        info!(
            "[{}] Channel reader complete: {} samples, {} edges sent",
            label, position, items_sent
        );

        sender.close();
        drop(sender);
        completed.fetch_add(1, Ordering::Relaxed);
    }

    /// Worker thread that reads one channel's block data and sends SampleBlocks.
    ///
    /// Instead of walking bits and sending per-edge Samples, this sends raw
    /// packed-bit blocks as `SampleBlock` — one send per ~16M samples. This is
    /// orders of magnitude faster for downstream consumers that can process
    /// packed bits directly (like the block-mode ParallelDecoder).
    fn block_reader_thread(config: BlockReaderConfig) {
        let BlockReaderConfig {
            archive,
            blocks,
            channel,
            header,
            sender,
            destination,
            max_samples,
            shutdown,
            completed,
        } = config;
        let label = channel_log_label(channel, destination.as_deref());
        let timestamp_step = (1_000_000_000.0 / header.samplerate_hz) as u64;
        let total_samples = max_samples
            .unwrap_or(header.total_samples)
            .min(header.total_samples);

        info!(
            "[{}] Starting block reader thread ({} samples, {} blocks)",
            label, total_samples, header.total_blocks
        );

        for block_num in 0..header.total_blocks {
            if shutdown.load(Ordering::Relaxed) {
                debug!("[{}] Block reader shutdown at block {}", label, block_num);
                break;
            }

            let block_start_position = block_num * header.samples_per_block;
            if block_start_position >= total_samples {
                break;
            }

            // Load block data (check cache first, then archive)
            let block_data = {
                let key = (channel, block_num);

                {
                    let cache_guard = blocks.lock().unwrap();
                    if let Some(data) = cache_guard.get(&key) {
                        Arc::clone(data)
                    } else {
                        drop(cache_guard);

                        let block_name = format!("L-{}/{}", channel, block_num);
                        let data = {
                            let mut archive_guard = archive.lock().unwrap();
                            let mut file = match archive_guard.by_name(&block_name) {
                                Ok(f) => f,
                                Err(_) => {
                                    debug!("[{}] Block {} not found, stopping", label, block_num);
                                    break;
                                }
                            };
                            let mut buf = Vec::new();
                            if file.read_to_end(&mut buf).is_err() {
                                debug!("[{}] Failed to read block {}", label, block_num);
                                break;
                            }
                            Arc::<[u8]>::from(buf)
                        };

                        let mut cache_guard = blocks.lock().unwrap();
                        cache_guard.insert(key, Arc::clone(&data));
                        data
                    }
                }
            };

            // Calculate how many samples are valid in this block
            let block_capacity = (block_data.len() * 8) as u64;
            let samples_in_block =
                block_capacity.min(total_samples - block_start_position) as usize;

            let sample_block = SampleBlock::new(
                block_data,
                block_start_position,
                samples_in_block,
                timestamp_step,
            );

            if sender.send(sample_block).is_err() {
                debug!(
                    "[{}] Block reader: all receivers disconnected at block {}",
                    label, block_num
                );
                completed.fetch_add(1, Ordering::Relaxed);
                return;
            }

            if block_num > 0 && block_num % 10 == 0 {
                let pct = ((block_start_position + samples_in_block as u64) as f64
                    / total_samples as f64)
                    * 100.0;
                debug!(
                    "[{}] Block progress: {:.1}% ({} blocks sent)",
                    label,
                    pct,
                    block_num + 1
                );
            }
        }

        info!("[{}] Block reader complete", label);

        sender.close();
        drop(sender);
        completed.fetch_add(1, Ordering::Relaxed);
    }
}

impl ProcessNode for DslFileSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn should_stop(&self) -> bool {
        self.threads_spawned && self.threads_completed.load(Ordering::Relaxed) >= self.num_threads
    }

    fn is_self_threading(&self) -> bool {
        true
    }

    fn num_inputs(&self) -> usize {
        0 // Source node
    }

    fn num_outputs(&self) -> usize {
        // One port per channel, `ch0..chN` — negotiates Sample vs
        // SampleBlock per connection (see `output_schema`'s
        // `with_sample_kinds`) instead of exposing separate `d`/`b` ports
        // for each.
        self.num_channels as usize
    }

    fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};

        (0..self.num_channels)
            .map(|i| {
                PortSchema::new::<Sample>(format!("ch{}", i), i as usize, PortDirection::Output)
                    // Every channel port aliases a raw file channel, so
                    // every port can be answered from the waveform index —
                    // prefer that, fall back to streaming for consumers (or
                    // live sources with no index) that don't ask for it.
                    .with_protocols(vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream])
                    // Block is a near-zero-cost passthrough of the on-disk
                    // block; Edge costs a real bit-walk to derive RLE edges
                    // (see `block_reader_thread`/`channel_reader_thread`
                    // below) — prefer Block, but a consumer that only wants
                    // Edge still gets it.
                    .with_sample_kinds(vec![SampleKind::Block, SampleKind::Edge])
            })
            .collect()
    }

    fn edge_query(
        &self,
        port: usize,
        _input_queries: &[Option<Arc<dyn EdgeQuery>>],
    ) -> Option<Arc<dyn EdgeQuery>> {
        let channel = port;
        let sampler = self.edge_index_handle()?;
        // Honor `with_max_samples` the same way the streaming reader
        // threads do, so a bounded source behaves identically regardless
        // of which protocol a connection negotiates.
        let total_samples = self
            .max_samples
            .unwrap_or(self.header.total_samples)
            .min(self.header.total_samples);
        Some(Arc::new(DslChannelEdgeIndex {
            sampler,
            channel,
            sample_period: self.header.sample_period,
            samplerate_hz: self.header.samplerate_hz,
            total_samples,
        }))
    }

    fn work(&mut self, _inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        use crate::runtime::node::WorkError;

        if self.threads_spawned {
            // Already started - this shouldn't be called again for self-threading nodes
            return Err(WorkError::NodeError(
                "work() called multiple times on self-threading node".to_string(),
            ));
        }

        // First and only call: spawn one thread per connected output destination
        self.threads_spawned = true;

        info!(
            "File source: Spawning per-destination threads for {} samples at {:.1} MHz ({} channels)",
            self.header.total_samples,
            self.header.samplerate_hz / 1_000_000.0,
            self.num_channels
        );

        // Collect all channel-destination pairs to spawn threads for
        // Each destination gets its own independent reader thread. Every
        // channel has exactly one output port now, but that port can
        // carry `Sample` and `SampleBlock` destinations simultaneously
        // (negotiated per connection — see `output_sample_kinds`), so
        // both queries run independently against the same port.
        let mut edge_thread_configs: Vec<(usize, usize, Sender<Sample>, Option<String>)> =
            Vec::new();
        let mut block_thread_configs: Vec<(usize, usize, Sender<SampleBlock>, Option<String>)> =
            Vec::new();

        for channel_idx in 0..self.num_channels as usize {
            let Some(port) = outputs.get(channel_idx) else {
                continue;
            };
            if let Some(senders) = port.split_senders::<Sample>() {
                for (dest_idx, sender) in senders.into_iter().enumerate() {
                    let destination = sender.destination_label().map(str::to_owned);
                    edge_thread_configs.push((channel_idx, dest_idx, sender, destination));
                }
            }
            if let Some(senders) = port.split_senders::<SampleBlock>() {
                for (dest_idx, sender) in senders.into_iter().enumerate() {
                    let destination = sender.destination_label().map(str::to_owned);
                    block_thread_configs.push((channel_idx, dest_idx, sender, destination));
                }
            }
        }

        let mut handles = Vec::new();

        // Spawn edge reader threads
        for (channel_idx, dest_idx, sender, destination) in edge_thread_configs.into_iter() {
            let archive = Arc::clone(&self.archive);
            let blocks = Arc::clone(&self.blocks);
            let header = self.header.clone();
            let max_samples = self.max_samples;
            let shutdown = Arc::clone(&self.shutdown);
            let completed = Arc::clone(&self.threads_completed);

            let handle = std::thread::Builder::new()
                .name(format!("dsl_ch{}_edge_dest{}", channel_idx, dest_idx))
                .spawn(move || {
                    Self::channel_reader_thread(ChannelReaderConfig {
                        archive,
                        blocks,
                        channel: channel_idx,
                        header,
                        sender,
                        destination,
                        max_samples,
                        shutdown,
                        completed,
                    });
                })
                .expect("Failed to spawn DslFileSource edge reader thread");

            handles.push(handle);
        }

        // Spawn block reader threads
        for (channel_idx, dest_idx, sender, destination) in block_thread_configs.into_iter() {
            let archive = Arc::clone(&self.archive);
            let blocks = Arc::clone(&self.blocks);
            let header = self.header.clone();
            let max_samples = self.max_samples;
            let shutdown = Arc::clone(&self.shutdown);
            let completed = Arc::clone(&self.threads_completed);

            let handle = std::thread::Builder::new()
                .name(format!("dsl_ch{}_block_dest{}", channel_idx, dest_idx))
                .spawn(move || {
                    Self::block_reader_thread(BlockReaderConfig {
                        archive,
                        blocks,
                        channel: channel_idx,
                        header,
                        sender,
                        destination,
                        max_samples,
                        shutdown,
                        completed,
                    });
                })
                .expect("Failed to spawn DslFileSource block reader thread");

            handles.push(handle);
        }

        self.num_threads = handles.len();
        self.thread_handles = Some(handles);

        info!(
            "File source: Spawned {} reader threads ({} channels × destinations)",
            self.num_threads, self.num_channels
        );

        Ok(0)
    }
}

impl Drop for DslFileSource {
    fn drop(&mut self) {
        // Signal all threads to stop
        self.shutdown.store(true, Ordering::Relaxed);

        // Join all thread handles
        if let Some(handles) = self.thread_handles.take() {
            for handle in handles {
                let _ = handle.join();
            }
        }
    }
}

/// A [`DslFileSource`] whose file path arrives over a `filename` input (a
/// [`TextSample`] level) at run start instead of being known at build time
/// — the runtime behind a graph that *wires* the source's File socket
/// rather than picking a path in the node.
///
/// Opens the file on the filename level's guaranteed t=0 initial value
/// (level-stream contract: a bounded, one-time wait), then behaves exactly
/// like the inner source. Two deliberate constraints:
///
/// - **No `EdgeQuery`.** Protocol negotiation happens at build, before the
///   file (and thus its waveform index) exists, so every consumer streams.
///   A build-time path keeps skip-ahead queries — prefer it when the name
///   doesn't need to come from the graph.
/// - **One file per run.** Filename changes after the first value are not
///   consumed; a name level that keeps changing will eventually fill its
///   channel and backpressure its producer.
pub struct DeferredDslFileSource {
    name: String,
    num_channels: u8,
    max_samples: Option<u64>,
    name_buffer: VecDeque<TextSample>,
    inner: Option<DslFileSource>,
}

impl DeferredDslFileSource {
    pub fn new(num_channels: u8) -> Self {
        Self {
            name: "dsl_file_source".to_string(),
            num_channels,
            max_samples: None,
            name_buffer: VecDeque::new(),
            inner: None,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_max_samples(mut self, max_samples: Option<u64>) -> Self {
        self.max_samples = max_samples;
        self
    }
}

impl ProcessNode for DeferredDslFileSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn should_stop(&self) -> bool {
        self.inner.as_ref().is_some_and(|inner| inner.should_stop())
    }

    fn is_self_threading(&self) -> bool {
        true
    }

    fn num_inputs(&self) -> usize {
        1
    }

    fn num_outputs(&self) -> usize {
        self.num_channels as usize
    }

    fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};
        vec![PortSchema::new::<TextSample>(
            "filename",
            0,
            PortDirection::Input,
        )]
    }

    fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};

        // Mirrors `DslFileSource::output_schema` minus the `EdgeQuery`
        // protocol — the index can't answer queries before the file is
        // known, and negotiation happens at build (see the struct doc).
        (0..self.num_channels)
            .map(|i| {
                PortSchema::new::<Sample>(format!("ch{}", i), i as usize, PortDirection::Output)
                    .with_sample_kinds(vec![SampleKind::Block, SampleKind::Edge])
            })
            .collect()
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        use crate::runtime::node::WorkError;

        if self.inner.is_some() {
            return Err(WorkError::NodeError(
                "work() called multiple times on self-threading node".to_string(),
            ));
        }

        let mut names = inputs
            .first()
            .and_then(|port| port.get::<TextSample>(&mut self.name_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing filename input".to_string()))?;
        // Level-stream contract: the initial value arrives at t=0, so this
        // one-time blocking wait is bounded.
        let initial = names.recv()?;

        info!(
            "[{}] opening capture from wired filename: {:?}",
            self.name, initial.value
        );
        let inner = DslFileSource::new(&initial.value, self.num_channels)
            .map_err(|e| WorkError::NodeError(format!("cannot open '{}': {e}", initial.value)))?
            .with_name(self.name.clone())
            .with_max_samples(self.max_samples);
        self.inner = Some(inner);
        self.inner
            .as_mut()
            .expect("just assigned")
            .work(&[], outputs)
    }
}

// ============================================================================
// Per-channel thread function
// ============================================================================

/// Configuration for a per-channel reader thread
struct ChannelReaderConfig {
    archive: Arc<Mutex<ZipArchive<File>>>,
    blocks: BlockCache,
    channel: usize,
    header: DslHeader,
    sender: Sender<Sample>,
    destination: Option<String>,
    max_samples: Option<u64>,
    shutdown: Arc<AtomicBool>,
    completed: Arc<AtomicUsize>,
}

/// Configuration for a per-channel block reader thread
struct BlockReaderConfig {
    archive: Arc<Mutex<ZipArchive<File>>>,
    blocks: BlockCache,
    channel: usize,
    header: DslHeader,
    sender: Sender<SampleBlock>,
    destination: Option<String>,
    max_samples: Option<u64>,
    shutdown: Arc<AtomicBool>,
    completed: Arc<AtomicUsize>,
}

fn channel_log_label(channel: usize, destination: Option<&str>) -> String {
    match destination {
        Some(destination) if !destination.is_empty() => format!("ch{channel} -> {destination}"),
        _ => format!("ch{channel}"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::node::ProcessNode;

    // ── DeferredDslFileSource ────────────────────────────────────────────

    /// A nonexistent path arriving over the filename wire is a node error,
    /// reported with the offending path.
    #[test]
    fn deferred_source_reports_unopenable_file() {
        use crate::runtime::TextSample;
        use crate::runtime::node::WorkError;
        use crate::runtime::sender::ChannelMessage;
        use crate::runtime::watchdog::Watchdog;
        use crossbeam_channel::bounded;

        let wd = Watchdog::new();
        let (name_tx, name_rx) = bounded::<ChannelMessage<TextSample>>(4);
        name_tx
            .send(ChannelMessage::Sample(TextSample::new(
                "/nonexistent/capture.dsl",
                0,
            )))
            .unwrap();
        drop(name_tx);
        let inputs = [InputPort::new_with_watchdog(
            name_rx, &wd, "src", "filename",
        )];

        let mut source = DeferredDslFileSource::new(4);
        match source.work(&inputs, &[]) {
            Err(WorkError::NodeError(message)) => {
                assert!(message.contains("/nonexistent/capture.dsl"), "{message}");
            }
            other => panic!("expected a node error, got {other:?}"),
        }
    }

    /// A filename channel that closes without ever delivering a value
    /// shuts the source down instead of hanging or opening anything.
    #[test]
    fn deferred_source_shuts_down_on_closed_filename_channel() {
        use crate::runtime::TextSample;
        use crate::runtime::node::WorkError;
        use crate::runtime::sender::ChannelMessage;
        use crate::runtime::watchdog::Watchdog;
        use crossbeam_channel::bounded;

        let wd = Watchdog::new();
        let (name_tx, name_rx) = bounded::<ChannelMessage<TextSample>>(4);
        drop(name_tx);
        let inputs = [InputPort::new_with_watchdog(
            name_rx, &wd, "src", "filename",
        )];

        let mut source = DeferredDslFileSource::new(4);
        assert!(matches!(
            source.work(&inputs, &[]),
            Err(WorkError::Shutdown)
        ));
    }

    /// End-to-end (gated on the local capture): a pipeline whose filename
    /// travels over a wire streams the same leading edges as one whose path
    /// was known at build time.
    #[test]
    fn deferred_source_streams_the_named_file() {
        use crate::runtime::{Pipeline, TextSample};
        use std::sync::{Arc, Mutex};

        let path = std::path::Path::new("_captures/wipneus5.dsl");
        if !path.exists() {
            return;
        }
        const MAX_SAMPLES: u64 = 1_000_000;

        /// One-shot filename level source.
        struct NameSource {
            path: String,
            sent: bool,
        }
        impl ProcessNode for NameSource {
            fn name(&self) -> &str {
                "name_source"
            }
            fn num_inputs(&self) -> usize {
                0
            }
            fn num_outputs(&self) -> usize {
                1
            }
            fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
                use crate::runtime::ports::{PortDirection, PortSchema};
                vec![PortSchema::new::<TextSample>(
                    "text",
                    0,
                    PortDirection::Output,
                )]
            }
            fn work(&mut self, _: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
                use crate::runtime::node::WorkError;
                if self.sent {
                    return Err(WorkError::Shutdown);
                }
                self.sent = true;
                let out = outputs
                    .first()
                    .and_then(|p| p.get::<TextSample>())
                    .ok_or_else(|| WorkError::NodeError("missing output".into()))?;
                out.send(TextSample::new(self.path.clone(), 0))?;
                Ok(1)
            }
        }

        /// Collects `Sample` edges from one channel.
        struct CollectEdges(Arc<Mutex<Vec<Sample>>>);
        impl ProcessNode for CollectEdges {
            fn name(&self) -> &str {
                "collect"
            }
            fn num_inputs(&self) -> usize {
                1
            }
            fn num_outputs(&self) -> usize {
                0
            }
            fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
                use crate::runtime::ports::{PortDirection, PortSchema};
                vec![PortSchema::new::<Sample>("in", 0, PortDirection::Input)]
            }
            fn work(&mut self, inputs: &[InputPort], _: &[OutputPort]) -> WorkResult<usize> {
                use crate::runtime::node::WorkError;
                let mut buffer = std::collections::VecDeque::new();
                let mut input = inputs
                    .first()
                    .and_then(|p| p.get::<Sample>(&mut buffer))
                    .ok_or_else(|| WorkError::NodeError("missing input".into()))?;
                let sample = input.recv()?;
                self.0.lock().unwrap().push(sample);
                Ok(1)
            }
        }

        let run_deferred = || -> Vec<Sample> {
            let collected = Arc::new(Mutex::new(Vec::new()));
            let mut pipeline = Pipeline::new();
            pipeline
                .add_process(
                    "names",
                    NameSource {
                        path: path.display().to_string(),
                        sent: false,
                    },
                )
                .unwrap();
            pipeline
                .add_process(
                    "source",
                    DeferredDslFileSource::new(8).with_max_samples(Some(MAX_SAMPLES)),
                )
                .unwrap();
            pipeline
                .add_process("collect", CollectEdges(collected.clone()))
                .unwrap();
            pipeline
                .connect("names", "text", "source", "filename")
                .unwrap();
            pipeline.connect("source", "ch7", "collect", "in").unwrap();
            pipeline.build().unwrap().wait();
            Arc::try_unwrap(collected).unwrap().into_inner().unwrap()
        };

        let run_direct = || -> Vec<Sample> {
            let collected = Arc::new(Mutex::new(Vec::new()));
            let mut pipeline = Pipeline::new();
            pipeline
                .add_process(
                    "source",
                    DslFileSource::new(path, 8)
                        .unwrap()
                        .with_max_samples(Some(MAX_SAMPLES)),
                )
                .unwrap();
            pipeline
                .add_process("collect", CollectEdges(collected.clone()))
                .unwrap();
            pipeline.connect("source", "ch7", "collect", "in").unwrap();
            pipeline.build().unwrap().wait();
            Arc::try_unwrap(collected).unwrap().into_inner().unwrap()
        };

        let deferred = run_deferred();
        let direct = run_direct();
        assert!(!direct.is_empty(), "expected edges in the bounded prefix");
        assert_eq!(deferred, direct);
    }

    #[test]
    fn test_parse_sample_rate_valid() {
        assert_eq!(
            DslFileSource::parse_sample_rate("50 MHz"),
            Some(50_000_000.0)
        );
        assert_eq!(
            DslFileSource::parse_sample_rate("1 GHz"),
            Some(1_000_000_000.0)
        );
        assert_eq!(DslFileSource::parse_sample_rate("100 kHz"), Some(100_000.0));
        assert_eq!(DslFileSource::parse_sample_rate("100 KHz"), Some(100_000.0));
        assert_eq!(DslFileSource::parse_sample_rate("1000 Hz"), Some(1000.0));
        assert_eq!(
            DslFileSource::parse_sample_rate("2.5 MHz"),
            Some(2_500_000.0)
        );
    }

    #[test]
    fn test_parse_sample_rate_invalid() {
        assert_eq!(DslFileSource::parse_sample_rate("invalid"), None);
        assert_eq!(DslFileSource::parse_sample_rate("50"), None);
        assert_eq!(DslFileSource::parse_sample_rate("MHz 50"), None);
        assert_eq!(DslFileSource::parse_sample_rate("50 mhz"), None);
        assert_eq!(DslFileSource::parse_sample_rate(""), None);
        assert_eq!(DslFileSource::parse_sample_rate("abc MHz"), None);
    }

    #[test]
    fn test_get_bit() {
        let data = vec![0b10101010, 0b11001100];
        assert!(!DslFileSource::get_bit(&data, 0)); // bit 0 of byte 0
        assert!(DslFileSource::get_bit(&data, 1)); // bit 1 of byte 0
        assert!(!DslFileSource::get_bit(&data, 2)); // bit 2 of byte 0
        assert!(DslFileSource::get_bit(&data, 3)); // bit 3 of byte 0
        assert!(DslFileSource::get_bit(&data, 7)); // bit 7 of byte 0
        assert!(!DslFileSource::get_bit(&data, 8)); // bit 0 of byte 1
        assert!(!DslFileSource::get_bit(&data, 9)); // bit 1 of byte 1
        assert!(DslFileSource::get_bit(&data, 10)); // bit 2 of byte 1
        assert!(DslFileSource::get_bit(&data, 11)); // bit 3 of byte 1

        // Out of bounds
        assert!(!DslFileSource::get_bit(&data, 16));
        assert!(!DslFileSource::get_bit(&data, 100));
    }

    #[test]
    fn test_capture_reader_wipneus5_window_if_present() {
        let path = Path::new("_captures/wipneus5.dsl");
        if !path.exists() {
            return;
        }

        let mut reader = DslCaptureReader::open(path)
            .expect("wipneus5.dsl should open with the windowed reader")
            .with_max_cached_blocks(4);
        assert!(reader.header().total_samples > 0);
        assert!(reader.header().total_probes > 0);

        let channel_count = reader.header().total_probes.min(4);
        let channels: Vec<usize> = (0..channel_count).collect();
        let window = reader
            .sampled_window(&channels, 0, 100_000, 800)
            .expect("small wipneus5.dsl viewport should read");

        assert_eq!(window.channels.len(), channel_count);
        assert!(window.sample_step > 0);
    }

    #[test]
    fn test_dsl_channel_edge_index_matches_ground_truth() {
        let path = Path::new("_captures/wipneus5.dsl");
        if !path.exists() {
            return;
        }

        let source = DslFileSource::new(path, 1).expect("wipneus5.dsl should open");
        let edge_query = source
            .edge_query(0, &[])
            .expect("DslFileSource should provide an EdgeQuery for channel 0");

        // Ground truth: exact transitions over a bounded prefix, computed
        // directly against the index (bypassing the galloping wrapper) so
        // this validates next_edge's search logic against real data shape,
        // not just the index itself.
        let ground_truth_end = 2_000_000u64.min(edge_query.total_samples());
        let mut sampler = DslChunkedCaptureReader::open(path).expect("sampler should open");
        let window = sampler
            .sampled_window(&[0], 0, ground_truth_end, ground_truth_end as usize)
            .expect("exact window should read");
        let expected: Vec<(u64, bool)> = window.channels[0]
            .transitions
            .iter()
            .map(|t| (t.sample, t.value))
            .collect();

        // Walk next_edge from 0 and confirm it reproduces the same sequence
        // (exercises galloping across whatever gap sizes occur in the real
        // signal, small and large alike).
        let mut position = 0u64;
        let mut found = Vec::new();
        while let Some(t) = edge_query
            .next_edge(position, ground_truth_end)
            .expect("next_edge should not error")
        {
            found.push((t.sample, t.value));
            position = t.sample;
        }
        assert_eq!(found, expected);

        // value_at agrees with the transitions: the new value holds at/after
        // the edge, the old value holds strictly before it.
        for &(sample, value) in &expected {
            assert_eq!(edge_query.value_at(sample).unwrap(), value);
            if sample > 0 {
                assert_ne!(edge_query.value_at(sample - 1).unwrap(), value);
            }
        }
    }

    #[test]
    fn test_dsl_channel_edge_index_next_edge_with_value() {
        let path = Path::new("_captures/wipneus5.dsl");
        if !path.exists() {
            return;
        }
        let source = DslFileSource::new(path, 1).expect("wipneus5.dsl should open");
        let edge_query = source.edge_query(0, &[]).expect("edge query available");
        let limit = 2_000_000u64.min(edge_query.total_samples());

        let Some(first) = edge_query.next_edge(0, limit).unwrap() else {
            return; // channel 0 has no transitions in this prefix; nothing to check
        };

        let same = edge_query
            .next_edge_with_value(0, first.value, limit)
            .unwrap()
            .expect("the first transition itself satisfies its own value");
        assert_eq!(same, first);

        // Edges alternate, so the opposite value's first occurrence (if any
        // before `limit`) is strictly after `first`.
        if let Some(other) = edge_query
            .next_edge_with_value(0, !first.value, limit)
            .unwrap()
        {
            assert_ne!(other.value, first.value);
            assert!(other.sample > first.sample);
        }
    }

    #[test]
    fn test_dsl_channel_edge_index_end_of_file() {
        let path = Path::new("_captures/wipneus5.dsl");
        if !path.exists() {
            return;
        }
        let source = DslFileSource::new(path, 1).expect("wipneus5.dsl should open");
        let edge_query = source.edge_query(0, &[]).expect("edge query available");
        let total = edge_query.total_samples();

        assert_eq!(edge_query.next_edge(total - 1, total).unwrap(), None);
        assert_eq!(edge_query.next_edge(total, total).unwrap(), None);
    }

    #[test]
    fn test_dsl_file_source_new_valid() {
        // Test with actual scan.dsl file if it exists
        let result = DslFileSource::new("scan.dsl", 8);
        assert!(
            result.is_ok(),
            "Failed to create DslFileSource: {:?}",
            result.err()
        );

        if let Ok(source) = result {
            assert_eq!(source.num_channels(), 8);
            assert_eq!(source.num_inputs(), 0); // Source node
            assert_eq!(source.num_outputs(), 8); // one port per channel
            assert_eq!(source.name(), "dsl_file_source");

            // Check header parsing
            let header = source.header();
            assert!(header.total_probes > 0);
            assert!(header.total_samples > 0);
            assert!(header.samplerate_hz > 0.0);
            assert!(header.sample_period > 0.0);
        }
    }

    #[test]
    fn test_dsl_file_source_invalid_channels() {
        // Test with 0 channels
        let result = DslFileSource::new("scan.dsl", 0);
        assert!(result.is_err());

        // Test with too many channels (17)
        let result = DslFileSource::new("scan.dsl", 17);
        assert!(result.is_err());
    }

    #[test]
    fn test_dsl_file_source_invalid_file() {
        let result = DslFileSource::new("nonexistent.dsl", 8);
        assert!(result.is_err());
    }

    #[test]
    fn test_dsl_file_source_more_channels_than_file() {
        // scan.dsl has 11 channels, request 16
        let result = DslFileSource::new("scan.dsl", 16);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("has only"));
        }
    }

    #[test]
    fn test_dsl_file_source_builder_methods() {
        let result = DslFileSource::new("scan.dsl", 4);
        assert!(result.is_ok());

        if let Ok(source) = result {
            let source = source.with_name("custom_source");

            assert_eq!(source.name(), "custom_source");
        }
    }

    #[test]
    fn test_dsl_file_source_getters() {
        let result = DslFileSource::new("scan.dsl", 8);
        assert!(result.is_ok());

        if let Ok(source) = result {
            assert!(source.total_probes() > 0);
            assert!(source.total_samples() > 0);
            assert!(source.samplerate_hz() > 0.0);
            assert!(source.sample_period() > 0.0);
            assert!(source.capture_duration() > 0.0);

            // Verify relationships
            let expected_duration = source.total_samples() as f64 * source.sample_period();
            assert!((source.capture_duration() - expected_duration).abs() < 0.0001);
        }
    }

    #[test]
    fn test_dsl_file_source_worknode_methods() {
        let result = DslFileSource::new("scan.dsl", 8);
        assert!(result.is_ok());

        if let Ok(source) = result {
            // Should not be stopped initially (no threads spawned yet)
            assert!(!source.should_stop());

            // After marking spawned with 0 threads completed, still shouldn't stop
            // (threads_spawned is false initially)
            assert!(!source.threads_spawned);
        }
    }

    #[test]
    fn test_dsl_file_source_read_bit_valid() {
        let result = DslFileSource::new("scan.dsl", 8);
        assert!(result.is_ok());

        if let Ok(source) = result {
            // Read first bit from first channel
            let bit_result = source.read_bit(0, 0);
            assert!(
                bit_result.is_ok(),
                "Failed to read bit: {:?}",
                bit_result.err()
            );

            // Read from another channel
            let bit_result = source.read_bit(5, 100);
            assert!(bit_result.is_ok());
        }
    }

    #[test]
    fn test_dsl_file_source_read_bit_invalid_channel() {
        let result = DslFileSource::new("scan.dsl", 8);
        assert!(result.is_ok());

        if let Ok(source) = result {
            // Try to read from channel beyond total_probes
            let bit_result = source.read_bit(99, 0);
            assert!(bit_result.is_err());

            if let Err(e) = bit_result {
                match e {
                    Error::InvalidProbe(_) => {}
                    _ => panic!("Expected InvalidProbe error, got {:?}", e),
                }
            }
        }
    }

    #[test]
    fn test_dsl_file_source_read_bit_invalid_position() {
        let result = DslFileSource::new("scan.dsl", 8);
        assert!(result.is_ok());

        if let Ok(source) = result {
            // Try to read beyond total_samples
            let bit_result = source.read_bit(0, u64::MAX);
            assert!(bit_result.is_err());

            if let Err(e) = bit_result {
                match e {
                    Error::OutOfBounds(_) => {}
                    _ => panic!("Expected OutOfBounds error, got {:?}", e),
                }
            }
        }
    }

    #[test]
    fn test_dsl_file_source_header_fields() {
        let result = DslFileSource::new("scan.dsl", 8);
        assert!(result.is_ok());

        if let Ok(source) = result {
            let header = source.header();

            // Verify header fields are populated
            assert!(header.total_probes >= 8);
            assert!(header.total_samples > 0);
            assert!(header.total_blocks > 0);
            assert!(header.samples_per_block > 0);
            assert!(!header.samplerate.is_empty());
            assert!(header.samplerate_hz > 0.0);
            assert!(header.sample_period > 0.0);
            assert!(header.probe_names.len() == header.total_probes);

            // Verify sample rate calculation
            let expected_period = 1.0 / header.samplerate_hz;
            assert!((header.sample_period - expected_period).abs() < 1e-10);

            // Verify samples per block is the actual block size (typically 2^24 = 16777216)
            // This should be larger than the average (total_samples / total_blocks)
            // because the last block is typically shorter
            let average_per_block = header.total_samples / header.total_blocks;
            assert!(header.samples_per_block >= average_per_block);
            // Verify it's a reasonable block size (power of 2 for standard DSL format)
            assert_eq!(header.samples_per_block, 16777216); // 2^24 for scan.dsl
        }
    }

    #[test]
    fn test_dsl_file_source_channel_count_validation() {
        // Test minimum valid (1 channel)
        let result = DslFileSource::new("scan.dsl", 1);
        assert!(result.is_ok());
        if let Ok(source) = result {
            assert_eq!(source.num_channels(), 1);
            assert_eq!(source.num_outputs(), 1); // one port per channel
        }

        // Test maximum valid within file's channels (11)
        let result = DslFileSource::new("scan.dsl", 11);
        assert!(result.is_ok());
        if let Ok(source) = result {
            assert_eq!(source.num_channels(), 11);
            assert_eq!(source.num_outputs(), 11); // one port per channel
        }
    }

    #[test]
    fn test_dsl_file_source_block_caching() {
        let result = DslFileSource::new("scan.dsl", 8);
        assert!(result.is_ok());

        if let Ok(source) = result {
            // Read same bit twice - second read should use cache
            let bit1 = source.read_bit(0, 0);
            let bit2 = source.read_bit(0, 0);

            assert!(bit1.is_ok());
            assert!(bit2.is_ok());
            assert_eq!(bit1.unwrap(), bit2.unwrap());

            // Cache should have entry
            let cache = source.blocks.lock().unwrap();
            assert!(!cache.is_empty(), "Cache should not be empty after reads");
        }
    }

    #[test]
    fn test_dsl_file_source_multiple_channels() {
        let result = DslFileSource::new("scan.dsl", 8);
        assert!(result.is_ok());

        if let Ok(source) = result {
            // Read same position from multiple channels
            let mut channel_values = Vec::new();
            for ch in 0..8 {
                let bit_result = source.read_bit(ch, 1000);
                assert!(
                    bit_result.is_ok(),
                    "Failed to read channel {}: {:?}",
                    ch,
                    bit_result.err()
                );
                channel_values.push(bit_result.unwrap());
            }

            // Should be able to read from all channels
            assert_eq!(channel_values.len(), 8);
        }
    }
}
