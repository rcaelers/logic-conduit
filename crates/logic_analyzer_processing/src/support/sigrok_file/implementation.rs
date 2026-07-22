use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zip::ZipArchive;

use signal_processing::capture::{
    BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureMetadata,
    CaptureSource,
};
use signal_processing::{Error, Result};

/// Decoded sigrok capture data shared by a file source and random-access reader.
pub(crate) struct SigrokCapture {
    header: CaptureMetadata,
    samples: Arc<[u8]>,
    unitsize: usize,
}

impl SigrokCapture {
    pub(crate) fn open(path: impl AsRef<Path>, minimum_channels: u8) -> Result<Self> {
        if !(1..=32).contains(&minimum_channels) {
            return Err(Error::ParseError(format!(
                "num_channels must be 1-32, got {minimum_channels}"
            )));
        }

        let mut archive = ZipArchive::new(File::open(path)?).map_err(zip_error)?;
        let version = read_zip_text(&mut archive, "version")?;
        if version.trim() != "2" {
            return Err(Error::ParseError(format!(
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
            .ok_or_else(|| {
                Error::ParseError("missing required field: device X.capturefile".into())
            })?;
        let values = device.1;
        let capturefile = required(values, "capturefile")?;
        let total_probes: usize = required(values, "total probes")?
            .parse()
            .map_err(|_| Error::ParseError("invalid device X.total probes".to_string()))?;
        let unitsize: usize = required(values, "unitsize")?
            .parse()
            .map_err(|_| Error::ParseError("invalid device X.unitsize".to_string()))?;
        if unitsize == 0 || total_probes == 0 || total_probes > unitsize * 8 {
            return Err(Error::ParseError(format!(
                "invalid sigrok logic layout: {total_probes} probes in {unitsize}-byte samples"
            )));
        }
        if total_probes < minimum_channels as usize {
            return Err(Error::ParseError(format!(
                "File has only {total_probes} channels, need at least {minimum_channels}"
            )));
        }

        let samplerate = required(values, "samplerate")?.to_string();
        let samplerate_hz = parse_sample_rate(&samplerate)
            .ok_or_else(|| Error::ParseError(format!("Invalid sample rate: {samplerate}")))?;
        let trigger_sample = values
            .get("trigger sample")
            .map(|sample| {
                sample
                    .parse::<u64>()
                    .map_err(|_| Error::ParseError("invalid device X.trigger sample".to_string()))
            })
            .transpose()?;

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
            return Err(Error::ParseError(format!(
                "no {capturefile} logic data found"
            )));
        }
        let mut samples = Vec::new();
        for entry in logic_entries {
            let mut logic = archive.by_name(&entry).map_err(zip_error)?;
            logic.read_to_end(&mut samples)?;
        }
        if samples.len() % unitsize != 0 {
            return Err(Error::ParseError(format!(
                "logic data size {} is not divisible by unitsize {unitsize}",
                samples.len()
            )));
        }
        let total_samples = (samples.len() / unitsize) as u64;
        if total_samples == 0 {
            return Err(Error::ParseError(
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
        Ok(Self {
            header: CaptureMetadata {
                total_probes,
                samplerate,
                samplerate_hz,
                sample_period: 1.0 / samplerate_hz,
                total_samples,
                total_blocks: 1,
                samples_per_block: total_samples,
                probe_names,
                trigger_sample,
            },
            samples: Arc::from(samples),
            unitsize,
        })
    }

    pub(crate) fn metadata(&self) -> &CaptureMetadata {
        &self.header
    }
    pub(crate) fn samples(&self) -> Arc<[u8]> {
        Arc::clone(&self.samples)
    }
    pub(crate) fn unitsize(&self) -> usize {
        self.unitsize
    }
    fn value_at(&self, channel: usize, position: usize) -> bool {
        self.samples[position * self.unitsize + channel / 8] & (1 << (channel % 8)) != 0
    }
}

/// Random-access reader for a sigrok v2 logic capture.
pub(crate) struct SigrokCaptureReader {
    capture: SigrokCapture,
}

impl SigrokCaptureReader {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            capture: SigrokCapture::open(path, 1)?,
        })
    }
}
impl CaptureSource for SigrokCaptureReader {
    fn metadata(&self) -> &CaptureMetadata {
        self.capture.metadata()
    }
    fn read_sample(&mut self, channel: usize, position: u64) -> Result<bool> {
        if channel >= self.capture.metadata().total_probes {
            return Err(Error::InvalidProbe(channel));
        }
        if position >= self.capture.metadata().total_samples {
            return Err(Error::OutOfBounds(position));
        }
        Ok(self.capture.value_at(channel, position as usize))
    }
}
impl BlockCaptureSource for SigrokCaptureReader {
    fn read_packed_block(&mut self, channel: usize, block: u64) -> Result<BlockData> {
        if channel >= self.capture.metadata().total_probes {
            return Err(Error::InvalidProbe(channel));
        }
        if block != 0 {
            return Err(Error::InvalidBlock(block));
        }
        let samples = self.capture.metadata().total_samples as usize;
        let mut packed = vec![0_u8; samples.div_ceil(8)];
        for sample in 0..samples {
            if self.capture.value_at(channel, sample) {
                packed[sample / 8] |= 1 << (sample % 8);
            }
        }
        Ok(BlockData::from(packed))
    }
}

/// Indexable sigrok v2 capture data for the logic-analyzer viewer.
#[derive(Debug, Clone)]
pub(crate) struct SigrokFileCaptureDataSource {
    path: PathBuf,
    header: CaptureMetadata,
    source_len: u64,
}
impl SigrokFileCaptureDataSource {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let source_len = std::fs::metadata(&path)?.len();
        let header = SigrokCapture::open(&path, 1)?.metadata().clone();
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
    fn metadata(&self) -> &CaptureMetadata {
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

fn sigrok_sidecar_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.idx",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("capture.sr")
    ))
}
fn zip_error(error: zip::result::ZipError) -> Error {
    Error::ParseError(format!("capture archive error: {error}"))
}
fn read_zip_text(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let mut file = archive
        .by_name(name)
        .map_err(|_| Error::ParseError(format!("missing required field: {name}")))?;
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
        .ok_or_else(|| Error::ParseError(format!("missing required field: device X.{key}")))
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
