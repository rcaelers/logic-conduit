//! Sigrok v2 (`.sr`) processing-node file source.
//!
//! Sigrok session files are ZIP archives containing INI metadata and one or
//! more interleaved `logic-*` sample files.  This source decodes the logic
//! samples into level changes for each selected probe.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;

use zip::ZipArchive;

use signal_processing::errors::{WorkError, WorkResult};
use signal_processing::node::{ProcessNode};
use signal_processing::ports::{InputPort, OutputPort};
use signal_processing::ports::{PortDirection, PortSchema};
use signal_processing::sample::Sample;
use signal_processing::capture::{BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureSource};
use signal_processing::sender::{Sender};
use signal_processing::{DslHeader, Error, Result};

/// A PulseView/sigrok v2 session source.  Sigrok stores complete sample words
/// (unlike DSLogic's per-channel bit blocks), so this reader keeps the
/// decompressed logic stream in memory and exposes edge streams.
pub struct SigrokFileSource {
    name: String,
    header: DslHeader,
    samples: Arc<[u8]>,
    unitsize: usize,
    num_channels: u8,
    shutdown: Arc<AtomicBool>,
    completed: Arc<AtomicUsize>,
    threads: Option<Vec<JoinHandle<()>>>,
    spawned: bool,
    num_threads: usize,
}

struct ChannelStream {
    samples: Arc<[u8]>,
    unitsize: usize,
    channel: usize,
    total_samples: usize,
    timestamp_step: u64,
    sender: Sender<Sample>,
    shutdown: Arc<AtomicBool>,
    completed: Arc<AtomicUsize>,
}

impl ChannelStream {
    fn run(self) {
        let value_at = |sample| {
            self.samples[sample * self.unitsize + self.channel / 8]
                & (1 << (self.channel % 8))
                != 0
        };
        let mut current = value_at(0);
        if self.sender.send(Sample::new(current, 0)).is_ok() {
            for sample in 1..self.total_samples {
                if self.shutdown.load(Ordering::Relaxed) {
                    break;
                }
                let value = value_at(sample);
                if value != current {
                    current = value;
                    if self
                        .sender
                        .send(Sample::new(value, sample as u64 * self.timestamp_step))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        self.sender.close();
        self.completed.fetch_add(1, Ordering::Relaxed);
    }
}

/// Random-access reader for a sigrok v2 logic capture.
pub struct SigrokCaptureReader {
    source: SigrokFileSource,
}

impl SigrokCaptureReader {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let source = SigrokFileSource::new(path, 1)?;
        Ok(Self { source })
    }
}

impl CaptureSource for SigrokCaptureReader {
    fn metadata(&self) -> &DslHeader {
        &self.source.header
    }

    fn read_sample(&mut self, channel: usize, position: u64) -> Result<bool> {
        if channel >= self.source.header.total_probes {
            return Err(Error::InvalidProbe(channel));
        }
        if position >= self.source.header.total_samples {
            return Err(Error::OutOfBounds(position));
        }
        let byte = self.source.samples[position as usize * self.source.unitsize + channel / 8];
        Ok(byte & (1 << (channel % 8)) != 0)
    }
}

impl BlockCaptureSource for SigrokCaptureReader {
    fn read_packed_block(&mut self, channel: usize, block: u64) -> Result<BlockData> {
        if channel >= self.source.header.total_probes {
            return Err(Error::InvalidProbe(channel));
        }
        if block != 0 {
            return Err(Error::InvalidBlock(block));
        }
        let samples = self.source.header.total_samples as usize;
        let mut packed = vec![0_u8; samples.div_ceil(8)];
        for sample in 0..samples {
            let byte = self.source.samples[sample * self.source.unitsize + channel / 8];
            if byte & (1 << (channel % 8)) != 0 {
                packed[sample / 8] |= 1 << (sample % 8);
            }
        }
        Ok(BlockData::from(packed))
    }
}

/// Indexable sigrok v2 capture data for the logic-analyzer viewer.
#[derive(Debug, Clone)]
pub struct SigrokFileCaptureDataSource {
    path: PathBuf,
    header: DslHeader,
    source_len: u64,
}

impl SigrokFileCaptureDataSource {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let source_len = std::fs::metadata(&path)?.len();
        let header = SigrokFileSource::new(&path, 1)?.header.clone();
        Ok(Self {
            path,
            header,
            source_len,
        })
    }
}

impl CaptureDataSource for SigrokFileCaptureDataSource {
    type Reader = SigrokCaptureReader;

    fn open_reader(&self) -> Result<Self::Reader> {
        SigrokCaptureReader::open(&self.path)
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
        Some(sigrok_sidecar_path(&self.path))
    }
    fn display_name(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("capture.sr")
            .to_string()
    }
}

pub type SigrokChunkedCaptureReader = signal_processing::IndexSampler<SigrokCaptureReader>;

pub fn open_sigrok_chunked_capture<P: AsRef<Path>>(
    path: P,
) -> Result<SigrokChunkedCaptureReader> {
    signal_processing::IndexSampler::open_data_source(SigrokFileCaptureDataSource::open(path)?)
}

fn sigrok_sidecar_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("capture.sr")
        .to_string();
    name.push_str(".idx");
    path.with_file_name(name)
}

impl SigrokFileSource {
    pub fn new<P: AsRef<Path>>(path: P, num_channels: u8) -> Result<Self> {
        if !(1..=32).contains(&num_channels) {
            return Err(Error::ParseError(format!(
                "num_channels must be 1-32, got {num_channels}"
            )));
        }

        let mut archive = ZipArchive::new(File::open(path)?)?;
        let version = read_zip_text(&mut archive, "version")?;
        if version.trim() != "2" {
            return Err(Error::ParseHeader(format!(
                "unsupported sigrok session version '{}' (expected 2)",
                version.trim()
            )));
        }

        let metadata = parse_ini(&read_zip_text(&mut archive, "metadata")?);
        let device = metadata
            .iter()
            .find(|(section, values)| {
                section.starts_with("device ") && values.contains_key("capturefile")
            })
            .ok_or_else(|| Error::MissingField("device X.capturefile".to_string()))?;
        let values = device.1;
        let capturefile = required(values, "capturefile")?;
        let total_probes: usize = required(values, "total probes")?
            .parse()
            .map_err(|_| Error::ParseHeader("invalid device X.total probes".to_string()))?;
        let unitsize: usize = required(values, "unitsize")?
            .parse()
            .map_err(|_| Error::ParseHeader("invalid device X.unitsize".to_string()))?;
        if unitsize == 0 || total_probes == 0 || total_probes > unitsize * 8 {
            return Err(Error::ParseHeader(format!(
                "invalid sigrok logic layout: {total_probes} probes in {unitsize}-byte samples"
            )));
        }
        if total_probes < num_channels as usize {
            return Err(Error::ParseError(format!(
                "File has only {total_probes} channels, need at least {num_channels}"
            )));
        }

        let samplerate = required(values, "samplerate")?.to_string();
        let samplerate_hz = parse_sample_rate(&samplerate)
            .ok_or_else(|| Error::ParseHeader(format!("Invalid sample rate: {samplerate}")))?;

        let mut logic_entries: Vec<String> = archive
            .file_names()
            .filter(|name| {
                *name == capturefile
                    || name
                        .strip_prefix(&format!("{capturefile}-"))
                        .is_some_and(|suffix| suffix.parse::<u64>().is_ok())
            })
            .map(str::to_owned)
            .collect();
        logic_entries.sort_by_key(|name| {
            name.strip_prefix(&format!("{capturefile}-"))
                .and_then(|suffix| suffix.parse::<u64>().ok())
                .unwrap_or(0)
        });
        if logic_entries.is_empty() {
            return Err(Error::ParseHeader(format!(
                "no {capturefile} logic data found"
            )));
        }

        let mut samples = Vec::new();
        for entry in logic_entries {
            let mut logic = archive.by_name(&entry)?;
            logic.read_to_end(&mut samples)?;
        }
        if samples.len() % unitsize != 0 {
            return Err(Error::ParseHeader(format!(
                "logic data size {} is not divisible by unitsize {unitsize}",
                samples.len()
            )));
        }
        let total_samples = (samples.len() / unitsize) as u64;
        if total_samples == 0 {
            return Err(Error::ParseHeader(
                "logic data contains no samples".to_string(),
            ));
        }

        let probe_names = (0..total_probes)
            .map(|probe| {
                values
                    .get(&format!("probe{}", probe + 1))
                    .cloned()
                    .unwrap_or_else(|| format!("Probe {probe}"))
            })
            .collect();
        let header = DslHeader {
            total_probes,
            samplerate,
            samplerate_hz,
            sample_period: 1.0 / samplerate_hz,
            total_samples,
            total_blocks: 1,
            samples_per_block: total_samples,
            probe_names,
        };

        Ok(Self {
            name: "sigrok_file_source".into(),
            header,
            samples: Arc::from(samples),
            unitsize,
            num_channels,
            shutdown: Arc::new(AtomicBool::new(false)),
            completed: Arc::new(AtomicUsize::new(0)),
            threads: None,
            spawned: false,
            num_threads: 0,
        })
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn header(&self) -> &DslHeader {
        &self.header
    }

}

impl ProcessNode for SigrokFileSource {
    fn name(&self) -> &str {
        &self.name
    }
    fn should_stop(&self) -> bool {
        self.spawned && self.completed.load(Ordering::Relaxed) >= self.num_threads
    }
    fn is_self_threading(&self) -> bool {
        true
    }
    fn num_inputs(&self) -> usize {
        0
    }
    fn num_outputs(&self) -> usize {
        self.num_channels as usize
    }
    fn output_schema(&self) -> Vec<PortSchema> {
        (0..self.num_channels)
            .map(|channel| {
                PortSchema::new::<Sample>(
                    format!("ch{channel}"),
                    channel as usize,
                    PortDirection::Output,
                )
            })
            .collect()
    }

    fn work(&mut self, _inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        if self.spawned {
            return Err(WorkError::NodeError(
                "work() called multiple times on sigrok file source".into(),
            ));
        }
        self.spawned = true;
        let timestamp_step = (1_000_000_000.0 / self.header.samplerate_hz) as u64;
        let mut threads = Vec::new();
        for channel in 0..self.num_channels as usize {
            let Some(senders) = outputs
                .get(channel)
                .and_then(|output| output.split_senders::<Sample>())
            else {
                continue;
            };
            for sender in senders {
                let samples = Arc::clone(&self.samples);
                let shutdown = Arc::clone(&self.shutdown);
                let completed = Arc::clone(&self.completed);
                let unitsize = self.unitsize;
                let total_samples = self.header.total_samples as usize;
                threads.push(std::thread::spawn(move || {
                    ChannelStream {
                        samples,
                        unitsize,
                        channel,
                        total_samples,
                        timestamp_step,
                        sender,
                        shutdown,
                        completed,
                    }
                    .run()
                }));
            }
        }
        self.num_threads = threads.len();
        self.threads = Some(threads);
        Ok(0)
    }
}

impl Drop for SigrokFileSource {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(threads) = self.threads.take() {
            for thread in threads {
                let _ = thread.join();
            }
        }
    }
}

fn read_zip_text(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let mut file = archive
        .by_name(name)
        .map_err(|_| Error::MissingField(name.to_string()))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

fn parse_ini(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut sections = BTreeMap::new();
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = name.to_string();
        } else if let Some((key, value)) = line.split_once('=') {
            sections
                .entry(section.clone())
                .or_insert_with(BTreeMap::new)
                .insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    sections
}

fn required<'a>(values: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    values
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| Error::MissingField(format!("device X.{key}")))
}

fn parse_sample_rate(rate: &str) -> Option<f64> {
    let mut parts = rate.split_whitespace();
    let value: f64 = parts.next()?.parse().ok()?;
    let multiplier = match parts.next()? {
        "GHz" => 1e9,
        "MHz" => 1e6,
        "KHz" | "kHz" => 1e3,
        "Hz" => 1.0,
        _ => return None,
    };
    Some(value * multiplier)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("hello.sr")).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("version", options).unwrap();
        archive.write_all(b"2").unwrap();
        archive.start_file("metadata", options).unwrap();
        archive.write_all(b"[device 1]\ncapturefile=logic-1\ntotal probes=8\nsamplerate=1 MHz\nprobe1=TX\nunitsize=1\n").unwrap();
        archive.start_file("logic-1-1", options).unwrap();
        archive.write_all(&[0, 1, 1, 0, 0, 1, 0, 1]).unwrap();
        archive.finish().unwrap();
        dir
    }

    #[test]
    fn opens_checked_in_pulseview_capture() {
        let dir = fixture();
        let source = SigrokFileSource::new(dir.path().join("hello.sr"), 8).unwrap();
        assert_eq!(source.header().total_probes, 8);
        assert_eq!(source.header().samplerate_hz, 1_000_000.0);
        assert_eq!(source.header().total_samples, 8);
        assert_eq!(source.header().probe_names[0], "TX");
    }

    #[test]
    fn builds_a_persistent_waveform_index() {
        let dir = fixture();
        let capture = dir.path().join("hello.sr");

        let source = SigrokFileCaptureDataSource::open(&capture).unwrap();
        let index_path = source.index_path().unwrap();
        let mut reader = open_sigrok_chunked_capture(&capture).unwrap();

        assert!(index_path.exists());
        assert_eq!(reader.header().total_samples, 8);
        assert!(reader.sampled_window(&[0], 0, 128, 64).is_ok());
    }
}
