use crate::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CaptureMetadata {
    /// Total number of probes/channels.
    pub total_probes: usize,
    /// Sample rate as a display string, e.g. "50 MHz".
    pub samplerate: String,
    /// Sample rate in Hz.
    pub samplerate_hz: f64,
    /// Sample period in seconds.
    pub sample_period: f64,
    /// Total number of samples currently available.
    ///
    /// For finite file captures this is final. For future live captures this can
    /// grow over time.
    pub total_samples: u64,
    /// Total number of packed data blocks currently available.
    pub total_blocks: u64,
    /// Samples per packed block.
    pub samples_per_block: u64,
    /// Probe names indexed by probe number.
    pub probe_names: Vec<String>,
}

impl CaptureMetadata {
    pub fn duration_us(&self) -> f64 {
        self.total_samples as f64 * 1_000_000.0 / self.samplerate_hz
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureTransition {
    pub sample: u64,
    pub value: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureWaveformSegment {
    Level {
        start_sample: u64,
        end_sample: u64,
        value: bool,
    },
    Edge {
        sample: u64,
        before: bool,
        after: bool,
    },
    Activity {
        start_sample: u64,
        end_sample: u64,
        first: bool,
        last: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSampledChannel {
    pub channel: usize,
    pub name: String,
    pub initial: bool,
    pub transitions: Vec<CaptureTransition>,
    pub waveform: Vec<CaptureWaveformSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSampledWindow {
    pub start_sample: u64,
    pub end_sample: u64,
    pub sample_step: u64,
    pub channels: Vec<CaptureSampledChannel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureFingerprint {
    /// Stable revision used to invalidate persistent indexes.
    ///
    /// File sources can use the file size or a stronger hash/mtime combination.
    /// Live sources normally should not be indexed with a persistent sidecar.
    pub revision: u64,
}

pub trait CaptureSource {
    fn metadata(&self) -> &CaptureMetadata;

    fn read_sample(&mut self, channel: usize, position: u64) -> Result<bool>;

    fn capture_duration_us(&self) -> f64 {
        self.metadata().duration_us()
    }

    fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> Result<CaptureSampledWindow> {
        let metadata = self.metadata().clone();
        let start_sample = start_sample.min(metadata.total_samples.saturating_sub(1));
        let end_sample = end_sample.clamp(start_sample + 1, metadata.total_samples);
        let samples = end_sample - start_sample;
        let target_points = target_points.max(1) as u64;
        let sample_step = samples.div_ceil(target_points).max(1);

        let mut sampled_channels = Vec::with_capacity(channels.len());
        for &channel in channels {
            if channel >= metadata.total_probes {
                return Err(crate::Error::InvalidProbe(channel));
            }

            let name = metadata
                .probe_names
                .get(channel)
                .cloned()
                .unwrap_or_else(|| format!("Probe{channel}"));
            let mut current = self.read_sample(channel, start_sample)?;
            let initial = current;
            let mut transitions = Vec::new();
            let mut sample = start_sample.saturating_add(sample_step);

            while sample < end_sample {
                let value = self.read_sample(channel, sample)?;
                if value != current {
                    current = value;
                    transitions.push(CaptureTransition { sample, value });
                }
                sample = sample.saturating_add(sample_step);
                if sample == u64::MAX {
                    break;
                }
            }

            sampled_channels.push(CaptureSampledChannel {
                channel,
                name,
                initial,
                transitions,
                waveform: Vec::new(),
            });
        }

        Ok(CaptureSampledWindow {
            start_sample,
            end_sample,
            sample_step,
            channels: sampled_channels,
        })
    }
}

/// Packed sample bytes for one block: either owned (e.g. freshly
/// decompressed from an archive) or a zero-copy view into a memory-mapped
/// cache file, where only the pages actually touched are faulted in.
pub enum BlockData {
    Owned(Arc<[u8]>),
    #[cfg(not(target_arch = "wasm32"))]
    Mapped {
        map: Arc<memmap2::Mmap>,
        offset: usize,
        len: usize,
    },
}

impl std::ops::Deref for BlockData {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            BlockData::Owned(data) => data,
            #[cfg(not(target_arch = "wasm32"))]
            BlockData::Mapped { map, offset, len } => &map[*offset..*offset + *len],
        }
    }
}

impl From<Arc<[u8]>> for BlockData {
    fn from(data: Arc<[u8]>) -> Self {
        BlockData::Owned(data)
    }
}

pub trait BlockCaptureSource: CaptureSource {
    fn read_packed_block(&mut self, channel: usize, block: u64) -> Result<BlockData>;
}

/// Reloadable provider for capture data.
///
/// File formats, live captures, and generated/test data should implement this
/// boundary. The indexer only uses this trait; it does not know how the source
/// is opened, reloaded, or backed.
pub trait CaptureDataSource: Clone + Send + Sync + 'static {
    type Reader: BlockCaptureSource + Send + 'static;

    /// Open a fresh reader for the current source revision.
    ///
    /// For finite files this usually opens the file. For live sources this can
    /// return a reader over the latest immutable snapshot or a reloadable
    /// source-specific view.
    fn open_reader(&self) -> Result<Self::Reader>;
    fn metadata(&self) -> &CaptureMetadata;
    fn fingerprint(&self) -> CaptureFingerprint;
    fn index_path(&self) -> Option<PathBuf>;
    fn display_name(&self) -> String;
}

/// Windowed access to an already-opened capture's sample data.
///
/// The only implementation ([`IndexSampler`](super::waveform_index::IndexSampler))
/// is native-only — it depends on a persistent on-disk index, which wasm has
/// no filesystem to hold. This trait exists so consumers (the viewer) can
/// hold `Box<dyn CaptureIndex>` on both targets: on wasm nothing ever
/// constructs one, so the field simply stays `None` and every call site that
/// already handles "no sampler yet" continues to work unchanged, without
/// needing to know why.
pub trait CaptureIndex {
    fn display_name(&self) -> String;
    fn index_path(&self) -> &Path;
    fn header(&self) -> &CaptureMetadata;
    fn capture_duration_us(&self) -> f64;
    fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> Result<CaptureSampledWindow>;
}

pub fn packed_bit(data: &[u8], bit_index: usize) -> bool {
    let byte_index = bit_index / 8;
    let bit_offset = bit_index % 8;
    data.get(byte_index)
        .is_some_and(|byte| (byte & (1 << bit_offset)) != 0)
}

pub type DslHeader = CaptureMetadata;
pub type DslTransition = CaptureTransition;
pub type DslSampledChannel = CaptureSampledChannel;
pub type DslSampledWindow = CaptureSampledWindow;
pub type DslWaveformSegment = CaptureWaveformSegment;
