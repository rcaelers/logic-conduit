//! Random-access DSLogic `.dsl` capture-file support.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use tracing::debug;
use zip::ZipArchive;

#[cfg(test)]
use signal_processing::capture::CaptureSampledWindow;
use signal_processing::capture::{
    BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureMetadata,
    CaptureSource,
};
use signal_processing::waveform_index::IndexSampler;
use signal_processing::{Error, Result};

fn zip_error(error: zip::result::ZipError) -> Error {
    Error::ParseError(format!("capture archive error: {error}"))
}

/// Windowed DSLogic capture reader for interactive viewers.
///
/// Unlike [`DslFileSource`], this reader is not a streaming graph source. It is
/// optimized for repeated random-access viewport reads and keeps only a bounded
/// number of packed-bit ZIP blocks in memory.
pub(crate) struct DslCaptureReader {
    archive: ZipArchive<File>,
    header: CaptureMetadata,
    cache: HashMap<(usize, u64), BlockData>,
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

    pub(crate) fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let mut archive = ZipArchive::new(file).map_err(zip_error)?;
        let header = parse_header(&mut archive)?;

        Ok(Self {
            archive,
            header,
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            max_cached_blocks: Self::DEFAULT_MAX_CACHED_BLOCKS,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_max_cached_blocks(mut self, max_cached_blocks: usize) -> Self {
        self.max_cached_blocks = max_cached_blocks.max(1);
        self.trim_cache();
        self
    }

    #[cfg(test)]
    pub(crate) fn header(&self) -> &CaptureMetadata {
        &self.header
    }

    #[cfg(test)]
    pub(crate) fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> Result<CaptureSampledWindow> {
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
        Ok(get_bit(&data, sample_in_block))
    }

    fn read_block_cached(&mut self, key: (usize, u64)) -> Result<BlockData> {
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
            BlockData::from(data)
        };

        self.cache.insert(key, data.clone());
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
    fn metadata(&self) -> &CaptureMetadata {
        &self.header
    }

    fn read_sample(&mut self, channel: usize, position: u64) -> Result<bool> {
        self.read_bit_cached(channel, position)
    }
}

impl BlockCaptureSource for DslCaptureReader {
    fn read_packed_block(&mut self, channel: usize, block: u64) -> Result<BlockData> {
        self.read_block_cached((channel, block))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DslFileCaptureDataSource {
    path: PathBuf,
    header: CaptureMetadata,
    source_len: u64,
    index_path: PathBuf,
}

impl DslFileCaptureDataSource {
    pub(crate) fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let source_len = fs::metadata(&path)?.len();
        let file = File::open(&path)?;
        let mut archive = ZipArchive::new(file).map_err(zip_error)?;
        let header = parse_header(&mut archive)?;
        let index_path = dsl_sidecar_path(&path);

        Ok(Self {
            path,
            header,
            source_len,
            index_path,
        })
    }
}

impl CaptureDataSource for DslFileCaptureDataSource {
    type Reader = DslCaptureReader;

    fn open_reader(&self) -> Result<Self::Reader> {
        DslCaptureReader::open(&self.path)
    }

    fn metadata(&self) -> &CaptureMetadata {
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

pub(crate) type DslChunkedCaptureReader = IndexSampler<DslCaptureReader>;

fn dsl_sidecar_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("capture.dsl")
        .to_string();
    name.push_str(".idx");
    path.with_file_name(name)
}
pub(crate) fn parse_header(archive: &mut ZipArchive<File>) -> Result<CaptureMetadata> {
    let mut header_file = archive
        .by_name("header")
        .map_err(|e| Error::ParseError(format!("Cannot find header file: {}", e)))?;

    let mut header_content = String::new();
    header_file.read_to_string(&mut header_content)?;
    drop(header_file); // Explicitly drop to release archive borrow

    let mut total_probes: Option<usize> = None;
    let mut samplerate: Option<String> = None;
    let mut total_samples: Option<u64> = None;
    let mut total_blocks: Option<u64> = None;
    let mut trigger_sample: Option<u64> = None;
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
        } else if let Some(value) = line.strip_prefix("trigger sample = ") {
            trigger_sample = value.parse().ok();
        } else if line.starts_with("probe")
            && let Some((probe_part, name)) = line.split_once(" = ")
            && let Some(num_str) = probe_part.strip_prefix("probe")
            && let Ok(probe_num) = num_str.parse::<usize>()
        {
            probe_names_map.insert(probe_num, name.to_string());
        }
    }

    let total_probes = total_probes
        .ok_or_else(|| Error::ParseError("missing required field: total probes".into()))?;
    let samplerate =
        samplerate.ok_or_else(|| Error::ParseError("missing required field: samplerate".into()))?;
    let total_samples = total_samples
        .ok_or_else(|| Error::ParseError("missing required field: total samples".into()))?;
    let total_blocks = total_blocks
        .ok_or_else(|| Error::ParseError("missing required field: total blocks".into()))?;

    let samplerate_hz = parse_sample_rate(&samplerate)
        .ok_or_else(|| Error::ParseError(format!("Invalid sample rate: {}", samplerate)))?;
    let sample_period = 1.0 / samplerate_hz;

    // ZIP metadata already contains the uncompressed byte count. Avoid
    // decompressing the first 2 MiB block just to discover its size.
    let samples_per_block = {
        let block_name = "L-0/0";
        let file = archive
            .by_name(block_name)
            .map_err(|_| Error::ParseError("Could not read first block".to_string()))?;
        file.size() * 8
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

    Ok(CaptureMetadata {
        total_probes,
        samplerate,
        samplerate_hz,
        sample_period,
        total_samples, // Use actual value from header file
        total_blocks,
        samples_per_block,
        probe_names,
        trigger_sample,
    })
}
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
pub(crate) fn parse_sample_rate(samplerate: &str) -> Option<f64> {
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
