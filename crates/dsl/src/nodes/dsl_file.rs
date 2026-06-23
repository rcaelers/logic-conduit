//! DSL file source
//!
//! Provides `DslFileSource` - a runtime process node that reads DSLogic .dsl capture files
//! and outputs Sample streams per channel (run-length encoded for efficiency).
//!
//! Each broadcast destination runs in its own independent reading thread, so a slow consumer
//! on one destination never blocks other destinations. All threads share a single ZipArchive
//! and block cache via `Arc<Mutex<..>>`.

use crate::runtime::Sender;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkResult};
use crate::runtime::sample::{Sample, SampleBlock};
use crate::{DslError, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tracing::{debug, info};
use zip::ZipArchive;

/// Header information from a DSL file
#[derive(Debug, Clone)]
pub struct DslHeader {
    /// Total number of probes/channels
    pub total_probes: usize,
    /// Sample rate as a string (e.g., "50 MHz")
    pub samplerate: String,
    /// Sample rate in Hz
    pub samplerate_hz: f64,
    /// Sample period in seconds (1 / sample_rate)
    pub sample_period: f64,
    /// Total number of samples captured
    pub total_samples: u64,
    /// Total number of data blocks
    pub total_blocks: u64,
    /// Samples per block (calculated)
    pub samples_per_block: u64,
    /// Probe names indexed by probe number (0-based)
    pub probe_names: Vec<String>,
}

type BlockCache = Arc<Mutex<HashMap<(usize, u64), Arc<[u8]>>>>;

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
}

impl DslFileSource {
    /// Create a new DSL file source from a file path
    pub fn new<P: AsRef<Path>>(path: P, num_channels: u8) -> Result<Self> {
        if !(1..=16).contains(&num_channels) {
            return Err(DslError::ParseError(format!(
                "num_channels must be 1-16, got {}",
                num_channels
            )));
        }

        let file = File::open(path)?;
        let mut archive = ZipArchive::new(file)?;
        let header = Self::parse_header(&mut archive)?;

        if header.total_probes < num_channels as usize {
            return Err(DslError::ParseError(format!(
                "File has only {} channels, need at least {}",
                header.total_probes, num_channels
            )));
        }

        Ok(Self {
            name: "dsl_file_source".to_string(),
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
        })
    }

    fn parse_header(archive: &mut ZipArchive<File>) -> Result<DslHeader> {
        let mut header_file = archive
            .by_name("header")
            .map_err(|e| DslError::ParseHeader(format!("Cannot find header file: {}", e)))?;

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
            total_probes.ok_or_else(|| DslError::MissingField("total probes".to_string()))?;
        let samplerate =
            samplerate.ok_or_else(|| DslError::MissingField("samplerate".to_string()))?;
        let total_samples =
            total_samples.ok_or_else(|| DslError::MissingField("total samples".to_string()))?;
        let total_blocks =
            total_blocks.ok_or_else(|| DslError::MissingField("total blocks".to_string()))?;

        let samplerate_hz = Self::parse_sample_rate(&samplerate)
            .ok_or_else(|| DslError::ParseHeader(format!("Invalid sample rate: {}", samplerate)))?;
        let sample_period = 1.0 / samplerate_hz;

        // Determine actual block size by reading the first block (blocks are fixed-size except last)
        let samples_per_block = {
            let block_name = "L-0/0";
            let mut file = archive
                .by_name(block_name)
                .map_err(|_| DslError::ParseHeader("Could not read first block".to_string()))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map_err(|_| {
                DslError::ParseHeader("Could not read first block data".to_string())
            })?;
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
            return Err(DslError::InvalidProbe(channel));
        }
        if position >= self.header.total_samples {
            return Err(DslError::OutOfBounds(position));
        }

        let block_num = position / self.header.samples_per_block;

        // Additional safety check: ensure block number is valid
        if block_num >= self.header.total_blocks {
            return Err(DslError::OutOfBounds(position));
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
                .map_err(|_| DslError::InvalidBlock(block_num))?;

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
    fn get_bit(data: &[u8], bit_index: usize) -> bool {
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
            max_samples,
            shutdown,
            completed,
        } = config;
        let timestamp_step = (1_000_000_000.0 / header.samplerate_hz) as u64;
        let total_samples = max_samples
            .unwrap_or(header.total_samples)
            .min(header.total_samples);

        let mut current_value = false;
        let mut value_start_time: u64 = 0;
        let mut position: u64 = 0;
        let mut items_sent: u64 = 0;

        info!(
            "[ch{}] Starting channel reader thread ({} samples, {} blocks)",
            channel, total_samples, header.total_blocks
        );

        for block_num in 0..header.total_blocks {
            if shutdown.load(Ordering::Relaxed) {
                debug!(
                    "[ch{}] Shutdown signal received at block {}",
                    channel, block_num
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
                                    debug!(
                                        "[ch{}] Block {} not found, stopping",
                                        channel, block_num
                                    );
                                    break;
                                }
                            };
                            let mut buf = Vec::new();
                            if file.read_to_end(&mut buf).is_err() {
                                debug!("[ch{}] Failed to read block {}", channel, block_num);
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
                            "[ch{}] All receivers disconnected at position {}",
                            channel, position
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
                    "[ch{}] Progress: {:.1}% ({} samples, {} edges sent)",
                    channel, pct, position, items_sent
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
            "[ch{}] Channel reader complete: {} samples, {} edges sent",
            channel, position, items_sent
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
            max_samples,
            shutdown,
            completed,
        } = config;
        let timestamp_step = (1_000_000_000.0 / header.samplerate_hz) as u64;
        let total_samples = max_samples
            .unwrap_or(header.total_samples)
            .min(header.total_samples);

        info!(
            "[ch{}] Starting block reader thread ({} samples, {} blocks)",
            channel, total_samples, header.total_blocks
        );

        for block_num in 0..header.total_blocks {
            if shutdown.load(Ordering::Relaxed) {
                debug!(
                    "[ch{}] Block reader shutdown at block {}",
                    channel, block_num
                );
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
                                    debug!(
                                        "[ch{}] Block {} not found, stopping",
                                        channel, block_num
                                    );
                                    break;
                                }
                            };
                            let mut buf = Vec::new();
                            if file.read_to_end(&mut buf).is_err() {
                                debug!("[ch{}] Failed to read block {}", channel, block_num);
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
                    "[ch{}] Block reader: all receivers disconnected at block {}",
                    channel, block_num
                );
                completed.fetch_add(1, Ordering::Relaxed);
                return;
            }

            if block_num > 0 && block_num % 10 == 0 {
                let pct = ((block_start_position + samples_in_block as u64) as f64
                    / total_samples as f64)
                    * 100.0;
                debug!(
                    "[ch{}] Block progress: {:.1}% ({} blocks sent)",
                    channel,
                    pct,
                    block_num + 1
                );
            }
        }

        info!("[ch{}] Block reader complete", channel);

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
        // Edge ports (d0..dN) + Block ports (b0..bN)
        self.num_channels as usize * 2
    }

    fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};

        let mut schemas: Vec<PortSchema> = (0..self.num_channels)
            .map(|i| {
                PortSchema::new::<Sample>(format!("d{}", i), i as usize, PortDirection::Output)
            })
            .collect();

        // Block output ports: b0, b1, ..., bN
        let offset = self.num_channels as usize;
        for i in 0..self.num_channels {
            schemas.push(PortSchema::new::<SampleBlock>(
                format!("b{}", i),
                offset + i as usize,
                PortDirection::Output,
            ));
        }

        schemas
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
        // Each destination gets its own independent reader thread
        let mut edge_thread_configs: Vec<(usize, usize, Sender<Sample>)> = Vec::new();
        let mut block_thread_configs: Vec<(usize, usize, Sender<SampleBlock>)> = Vec::new();

        // Edge outputs: ports 0..num_channels
        for channel_idx in 0..self.num_channels as usize {
            if let Some(senders) = outputs
                .get(channel_idx)
                .and_then(|port| port.split_senders::<Sample>())
            {
                for (dest_idx, sender) in senders.into_iter().enumerate() {
                    edge_thread_configs.push((channel_idx, dest_idx, sender));
                }
            }
        }

        // Block outputs: ports num_channels..2*num_channels
        let block_offset = self.num_channels as usize;
        for channel_idx in 0..self.num_channels as usize {
            if let Some(senders) = outputs
                .get(block_offset + channel_idx)
                .and_then(|port| port.split_senders::<SampleBlock>())
            {
                for (dest_idx, sender) in senders.into_iter().enumerate() {
                    block_thread_configs.push((channel_idx, dest_idx, sender));
                }
            }
        }

        let mut handles = Vec::new();

        // Spawn edge reader threads
        for (channel_idx, dest_idx, sender) in edge_thread_configs.into_iter() {
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
                        max_samples,
                        shutdown,
                        completed,
                    });
                })
                .expect("Failed to spawn DslFileSource edge reader thread");

            handles.push(handle);
        }

        // Spawn block reader threads
        for (channel_idx, dest_idx, sender) in block_thread_configs.into_iter() {
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
    max_samples: Option<u64>,
    shutdown: Arc<AtomicBool>,
    completed: Arc<AtomicUsize>,
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::node::ProcessNode;

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
            assert_eq!(source.num_outputs(), 16); // 8 edge + 8 block ports
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
                    DslError::InvalidProbe(_) => {}
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
                    DslError::OutOfBounds(_) => {}
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
            assert_eq!(source.num_outputs(), 2); // 1 edge + 1 block
        }

        // Test maximum valid within file's channels (11)
        let result = DslFileSource::new("scan.dsl", 11);
        assert!(result.is_ok());
        if let Ok(source) = result {
            assert_eq!(source.num_channels(), 11);
            assert_eq!(source.num_outputs(), 22); // 11 edge + 11 block
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
