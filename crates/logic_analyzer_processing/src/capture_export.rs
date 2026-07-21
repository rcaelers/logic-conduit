//! Streaming export of finalized raw capture sessions.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use thiserror::Error;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use signal_processing::{
    CaptureChunk, CaptureChunkPayload, CaptureCursorItem, CaptureStoreCursor, CaptureStoreError,
    NativeFinalizedCapture,
};

const DSL_SAMPLES_PER_BLOCK: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawCaptureExportFormat {
    Dsl,
    SigrokV2,
}

impl RawCaptureExportFormat {
    pub const fn descriptor(self) -> CaptureExportFormatDescriptor {
        match self {
            Self::Dsl => CaptureExportFormatDescriptor {
                label: "DSL capture",
                extension: "dsl",
                trigger_metadata: TriggerMetadataSupport::Native,
                derived_data: DerivedExportSupport::Unsupported,
            },
            Self::SigrokV2 => CaptureExportFormatDescriptor {
                label: "Sigrok session",
                extension: "sr",
                trigger_metadata: TriggerMetadataSupport::CompatibleExtension,
                derived_data: DerivedExportSupport::Unsupported,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureExportFormatDescriptor {
    pub label: &'static str,
    pub extension: &'static str,
    pub trigger_metadata: TriggerMetadataSupport,
    pub derived_data: DerivedExportSupport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerMetadataSupport {
    Native,
    /// The raw interchange remains standard v2 data. DSL preserves the trigger in an optional
    /// metadata key which conforming readers may ignore.
    CompatibleExtension,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivedExportSupport {
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureExportRequest {
    pub destination: PathBuf,
    pub format: RawCaptureExportFormat,
    pub overwrite: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureExportProgress {
    pub samples_written: u64,
    pub total_samples: u64,
}

pub trait CaptureExportObserver {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn on_progress(&mut self, _progress: CaptureExportProgress) {}
}

#[derive(Default)]
pub struct IgnoreCaptureExportProgress;

impl CaptureExportObserver for IgnoreCaptureExportProgress {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureExportWarning {
    PortableTriggerMetadataExtension,
    DerivedDataNotExported,
}

impl CaptureExportWarning {
    pub const fn message(self) -> &'static str {
        match self {
            Self::PortableTriggerMetadataExtension => {
                "the portable v2 format has no standard trigger-position field; the trigger was preserved in an optional compatible metadata key"
            }
            Self::DerivedDataNotExported => {
                "derived lanes are not supported by this export format and were not requested"
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureExportReport {
    pub destination: PathBuf,
    pub format: RawCaptureExportFormat,
    pub samples_written: u64,
    pub encoded_bytes: u64,
    pub warnings: Vec<CaptureExportWarning>,
}

#[derive(Debug, Error)]
pub enum CaptureExportError {
    #[error("capture export requires durable timeline metadata")]
    MissingTimelineMetadata,
    #[error("cannot export an empty raw capture")]
    EmptyCapture,
    #[error("capture export destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("capture export destination has no parent directory: {0}")]
    InvalidDestination(PathBuf),
    #[error("capture export was cancelled")]
    Cancelled,
    #[error("capture data is inconsistent: {0}")]
    InconsistentCapture(String),
    #[error(transparent)]
    Store(#[from] CaptureStoreError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
}

pub fn export_finalized_capture(
    capture: &NativeFinalizedCapture,
    request: &CaptureExportRequest,
    observer: &mut dyn CaptureExportObserver,
) -> Result<CaptureExportReport, CaptureExportError> {
    let manifest = capture.manifest();
    if manifest.committed_samples == 0 {
        return Err(CaptureExportError::EmptyCapture);
    }
    let metadata = capture
        .session_metadata()?
        .and_then(|metadata| metadata.timeline)
        .ok_or(CaptureExportError::MissingTimelineMetadata)?;
    if metadata.channel_names().len() != manifest.descriptor.channels().len() {
        return Err(CaptureExportError::InconsistentCapture(format!(
            "{} channel names describe {} stored channels",
            metadata.channel_names().len(),
            manifest.descriptor.channels().len()
        )));
    }
    if let Some(trigger_sample) = metadata.trigger_sample()
        && trigger_sample >= manifest.committed_samples
    {
        return Err(CaptureExportError::InconsistentCapture(format!(
            "trigger sample {trigger_sample} is outside the {}-sample capture",
            manifest.committed_samples
        )));
    }
    if request.destination.exists() && !request.overwrite {
        return Err(CaptureExportError::DestinationExists(
            request.destination.clone(),
        ));
    }
    let parent = request
        .destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(CaptureExportError::InvalidDestination(
            request.destination.clone(),
        ));
    }

    validate_channel_names(metadata.channel_names())?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    let mut warnings = Vec::new();
    {
        let mut archive = ZipWriter::new(temporary.as_file_mut());
        match request.format {
            RawCaptureExportFormat::Dsl => {
                write_dsl(capture, &metadata, &mut archive, observer)?;
            }
            RawCaptureExportFormat::SigrokV2 => {
                write_sigrok_v2(capture, &metadata, &mut archive, observer)?;
                if metadata.trigger_sample().is_some() {
                    warnings.push(CaptureExportWarning::PortableTriggerMetadataExtension);
                }
            }
        }
        archive.finish()?.sync_all()?;
    }
    if observer.is_cancelled() {
        return Err(CaptureExportError::Cancelled);
    }
    if request.destination.exists() {
        fs::remove_file(&request.destination)?;
    }
    temporary
        .persist(&request.destination)
        .map_err(|error| error.error)?;
    let encoded_bytes = fs::metadata(&request.destination)?.len();
    Ok(CaptureExportReport {
        destination: request.destination.clone(),
        format: request.format,
        samples_written: manifest.committed_samples,
        encoded_bytes,
        warnings,
    })
}

fn write_dsl(
    capture: &NativeFinalizedCapture,
    metadata: &signal_processing::CaptureTimelineMetadata,
    archive: &mut ZipWriter<&mut std::fs::File>,
    observer: &mut dyn CaptureExportObserver,
) -> Result<(), CaptureExportError> {
    let manifest = capture.manifest();
    let total_samples = manifest.committed_samples;
    let samples_per_block = DSL_SAMPLES_PER_BLOCK.min(total_samples).max(1);
    let total_blocks = total_samples.div_ceil(samples_per_block);
    let options = zip_options();
    archive.start_file("header", options)?;
    writeln!(archive, "total probes = {}", metadata.channel_names().len())?;
    writeln!(archive, "samplerate = {} Hz", metadata.sample_rate_hz())?;
    writeln!(archive, "total samples = {total_samples}")?;
    writeln!(archive, "total blocks = {total_blocks}")?;
    if let Some(trigger_sample) = metadata.trigger_sample() {
        writeln!(archive, "trigger sample = {trigger_sample}")?;
    }
    for (channel, name) in metadata.channel_names().iter().enumerate() {
        writeln!(archive, "probe{channel} = {name}")?;
    }

    let channel_count = metadata.channel_names().len();
    let block_bytes = usize::try_from(samples_per_block.div_ceil(8))
        .map_err(|_| CaptureExportError::InconsistentCapture("DSL block is too large".into()))?;
    let mut channel_data = (0..channel_count)
        .map(|_| vec![0_u8; block_bytes])
        .collect::<Vec<_>>();
    let mut block = 0_u64;
    let mut samples_in_block = 0_u64;
    let mut samples_written = 0_u64;
    let mut cursor = capture.open_cursor()?;
    loop {
        match cursor.next()? {
            CaptureCursorItem::Chunk(chunk) => {
                for relative_sample in 0..chunk.sample_count() {
                    if samples_written.is_multiple_of(16_384) && observer.is_cancelled() {
                        return Err(CaptureExportError::Cancelled);
                    }
                    let byte = usize::try_from(samples_in_block / 8).map_err(|_| {
                        CaptureExportError::InconsistentCapture(
                            "DSL output offset is too large".into(),
                        )
                    })?;
                    let mask = 1_u8 << (samples_in_block % 8);
                    for (channel, output) in channel_data.iter_mut().enumerate() {
                        if packed_level(&chunk, relative_sample, channel)? {
                            output[byte] |= mask;
                        }
                    }
                    samples_in_block += 1;
                    samples_written += 1;
                    if samples_in_block == samples_per_block {
                        write_dsl_block(archive, options, block, samples_in_block, &channel_data)?;
                        block += 1;
                        samples_in_block = 0;
                        channel_data.iter_mut().for_each(|data| data.fill(0));
                        observer.on_progress(CaptureExportProgress {
                            samples_written,
                            total_samples,
                        });
                    }
                }
            }
            CaptureCursorItem::Pending => {
                return Err(CaptureExportError::InconsistentCapture(
                    "finalized capture cursor reported pending data".into(),
                ));
            }
            CaptureCursorItem::End => break,
        }
    }
    if samples_in_block != 0 {
        write_dsl_block(archive, options, block, samples_in_block, &channel_data)?;
        observer.on_progress(CaptureExportProgress {
            samples_written,
            total_samples,
        });
    }
    validate_exported_extent(samples_written, total_samples)
}

fn write_dsl_block(
    archive: &mut ZipWriter<&mut std::fs::File>,
    options: SimpleFileOptions,
    block: u64,
    sample_count: u64,
    channel_data: &[Vec<u8>],
) -> Result<(), CaptureExportError> {
    let byte_count = usize::try_from(sample_count.div_ceil(8))
        .map_err(|_| CaptureExportError::InconsistentCapture("DSL block is too large".into()))?;
    for (channel, data) in channel_data.iter().enumerate() {
        archive.start_file(format!("L-{channel}/{block}"), options)?;
        archive.write_all(&data[..byte_count])?;
    }
    Ok(())
}

fn write_sigrok_v2(
    capture: &NativeFinalizedCapture,
    metadata: &signal_processing::CaptureTimelineMetadata,
    archive: &mut ZipWriter<&mut std::fs::File>,
    observer: &mut dyn CaptureExportObserver,
) -> Result<(), CaptureExportError> {
    let manifest = capture.manifest();
    let total_samples = manifest.committed_samples;
    let channel_count = metadata.channel_names().len();
    let unitsize = channel_count.div_ceil(8);
    let options = zip_options();
    archive.start_file("version", options)?;
    archive.write_all(b"2")?;
    archive.start_file("metadata", options)?;
    writeln!(archive, "[global]")?;
    writeln!(archive, "sigrok version=dsl")?;
    writeln!(archive, "[device 1]")?;
    writeln!(archive, "capturefile=logic-1")?;
    writeln!(archive, "total probes={channel_count}")?;
    writeln!(archive, "total analog=0")?;
    writeln!(archive, "samplerate={} Hz", metadata.sample_rate_hz())?;
    writeln!(archive, "unitsize={unitsize}")?;
    if let Some(trigger_sample) = metadata.trigger_sample() {
        writeln!(archive, "trigger sample={trigger_sample}")?;
    }
    for (channel, name) in metadata.channel_names().iter().enumerate() {
        writeln!(archive, "probe{}={name}", channel + 1)?;
    }

    let mut cursor = capture.open_cursor()?;
    let mut entry = 1_u64;
    let mut samples_written = 0_u64;
    loop {
        match cursor.next()? {
            CaptureCursorItem::Chunk(chunk) => {
                if observer.is_cancelled() {
                    return Err(CaptureExportError::Cancelled);
                }
                archive.start_file(format!("logic-1-{entry}"), options)?;
                write_sigrok_chunk(archive, &chunk, channel_count, unitsize, observer)?;
                samples_written = samples_written
                    .checked_add(chunk.sample_count())
                    .ok_or_else(|| {
                        CaptureExportError::InconsistentCapture("sample count overflow".into())
                    })?;
                entry += 1;
                observer.on_progress(CaptureExportProgress {
                    samples_written,
                    total_samples,
                });
            }
            CaptureCursorItem::Pending => {
                return Err(CaptureExportError::InconsistentCapture(
                    "finalized capture cursor reported pending data".into(),
                ));
            }
            CaptureCursorItem::End => break,
        }
    }
    validate_exported_extent(samples_written, total_samples)
}

fn write_sigrok_chunk(
    archive: &mut ZipWriter<&mut std::fs::File>,
    chunk: &CaptureChunk,
    channel_count: usize,
    unitsize: usize,
    observer: &dyn CaptureExportObserver,
) -> Result<(), CaptureExportError> {
    if channel_count.is_multiple_of(8)
        && let CaptureChunkPayload::PackedLsbFirst {
            bytes,
            bit_offset: 0,
        } = chunk.payload()
    {
        let byte_count = usize::try_from(chunk.sample_count())
            .ok()
            .and_then(|samples| samples.checked_mul(unitsize))
            .ok_or_else(|| {
                CaptureExportError::InconsistentCapture("portable chunk is too large".into())
            })?;
        let data = bytes.as_ref().get(..byte_count).ok_or_else(|| {
            CaptureExportError::InconsistentCapture("portable chunk payload is truncated".into())
        })?;
        archive.write_all(data)?;
        return Ok(());
    }

    let sample_count = usize::try_from(chunk.sample_count()).map_err(|_| {
        CaptureExportError::InconsistentCapture("portable chunk is too large".into())
    })?;
    let output_len = sample_count.checked_mul(unitsize).ok_or_else(|| {
        CaptureExportError::InconsistentCapture("portable chunk is too large".into())
    })?;
    let mut output = vec![0_u8; output_len];
    for sample in 0..sample_count {
        if sample.is_multiple_of(16_384) && observer.is_cancelled() {
            return Err(CaptureExportError::Cancelled);
        }
        for channel in 0..channel_count {
            if packed_level(chunk, sample as u64, channel)? {
                output[sample * unitsize + channel / 8] |= 1 << (channel % 8);
            }
        }
    }
    archive.write_all(&output)?;
    Ok(())
}

fn packed_level(
    chunk: &CaptureChunk,
    relative_sample: u64,
    channel: usize,
) -> Result<bool, CaptureExportError> {
    chunk.packed_level(relative_sample, channel).ok_or_else(|| {
        CaptureExportError::InconsistentCapture(format!(
            "chunk {} has no sample {relative_sample}, channel {channel}",
            chunk.sequence()
        ))
    })
}

fn validate_channel_names(channel_names: &[String]) -> Result<(), CaptureExportError> {
    if let Some(name) = channel_names
        .iter()
        .find(|name| name.contains(['\r', '\n']))
    {
        return Err(CaptureExportError::InconsistentCapture(format!(
            "channel name {name:?} contains a line break"
        )));
    }
    Ok(())
}

fn validate_exported_extent(
    samples_written: u64,
    total_samples: u64,
) -> Result<(), CaptureExportError> {
    if samples_written != total_samples {
        return Err(CaptureExportError::InconsistentCapture(format!(
            "cursor yielded {samples_written} samples, manifest declares {total_samples}"
        )));
    }
    Ok(())
}

fn zip_options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use signal_processing::{
        CaptureChannelId, CaptureChunk, CaptureChunkWriter, CaptureSessionId,
        CaptureSessionOutcome, CaptureSource, CaptureStoreDescriptor, CaptureTimelineMetadata,
        NativeCaptureStore, NativeCaptureStoreConfig,
    };

    use super::*;
    use crate::nodes::sources::{DslCaptureReader, SigrokCaptureReader};

    const LEVELS: [[bool; 3]; 10] = [
        [false, true, true],
        [true, false, true],
        [true, true, false],
        [false, false, false],
        [true, false, true],
        [false, true, false],
        [true, true, true],
        [false, false, true],
        [true, false, false],
        [false, true, true],
    ];

    struct CancelImmediately;

    impl CaptureExportObserver for CancelImmediately {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    fn capture(directory: &Path) -> NativeFinalizedCapture {
        let session_id = CaptureSessionId::new(7);
        let channels = Arc::from([
            CaptureChannelId::new("physical:3"),
            CaptureChannelId::new("physical:9"),
            CaptureChannelId::new("physical:12"),
        ]);
        let descriptor = CaptureStoreDescriptor::new(session_id, Arc::clone(&channels)).unwrap();
        let (store, mut writer) =
            NativeCaptureStore::create(NativeCaptureStoreConfig::new(directory, descriptor))
                .unwrap();
        store
            .write_timeline_metadata(
                CaptureTimelineMetadata::new(
                    12_500_000.0,
                    vec!["Clock".into(), "Data".into(), "Enable".into()],
                )
                .unwrap(),
            )
            .unwrap();
        let bit_offset = 3_u8;
        let mut bytes = vec![0_u8; (usize::from(bit_offset) + LEVELS.len() * 3).div_ceil(8)];
        for (sample, channels) in LEVELS.iter().enumerate() {
            for (channel, level) in channels.iter().enumerate() {
                if *level {
                    let bit = usize::from(bit_offset) + sample * 3 + channel;
                    bytes[bit / 8] |= 1 << (bit % 8);
                }
            }
        }
        writer
            .append(
                CaptureChunk::packed_lsb_first(
                    session_id,
                    0,
                    0,
                    LEVELS.len() as u64,
                    channels,
                    bytes,
                    bit_offset,
                )
                .unwrap(),
            )
            .unwrap();
        writer.finish().unwrap();
        drop(writer);
        store
            .finalize_with_details(CaptureSessionOutcome::Complete, Some(4), Some(4))
            .unwrap()
    }

    #[test]
    fn dsl_export_reopens_with_identical_timeline_and_samples() {
        let store_dir = tempfile::tempdir().unwrap();
        let capture = capture(store_dir.path());
        let output_dir = tempfile::tempdir().unwrap();
        let output = output_dir.path().join("capture.dsl");
        let report = export_finalized_capture(
            &capture,
            &CaptureExportRequest {
                destination: output.clone(),
                format: RawCaptureExportFormat::Dsl,
                overwrite: false,
            },
            &mut IgnoreCaptureExportProgress,
        )
        .unwrap();
        assert!(report.warnings.is_empty());

        let mut reader = DslCaptureReader::open(&output).unwrap();
        assert_eq!(reader.header().samplerate_hz, 12_500_000.0);
        assert_eq!(reader.header().probe_names, ["Clock", "Data", "Enable"]);
        assert_eq!(reader.header().trigger_sample, Some(4));
        assert_eq!(reader.header().total_samples, 10);
        for sample in 0..10 {
            for (channel, expected) in LEVELS[sample as usize].iter().enumerate() {
                assert_eq!(reader.read_sample(channel, sample).unwrap(), *expected);
            }
        }
    }

    #[test]
    fn portable_export_reopens_with_identical_timeline_and_samples() {
        let store_dir = tempfile::tempdir().unwrap();
        let capture = capture(store_dir.path());
        let output_dir = tempfile::tempdir().unwrap();
        let output = output_dir.path().join("capture.sr");
        let report = export_finalized_capture(
            &capture,
            &CaptureExportRequest {
                destination: output.clone(),
                format: RawCaptureExportFormat::SigrokV2,
                overwrite: false,
            },
            &mut IgnoreCaptureExportProgress,
        )
        .unwrap();
        assert_eq!(
            report.warnings,
            [CaptureExportWarning::PortableTriggerMetadataExtension]
        );

        let mut reader = SigrokCaptureReader::open(&output).unwrap();
        assert_eq!(reader.metadata().samplerate_hz, 12_500_000.0);
        assert_eq!(
            reader.metadata().probe_names[..3],
            ["Clock", "Data", "Enable"]
        );
        assert_eq!(reader.metadata().trigger_sample, Some(4));
        assert_eq!(reader.metadata().total_samples, 10);
        for sample in 0..10 {
            for (channel, expected) in LEVELS[sample as usize].iter().enumerate() {
                assert_eq!(reader.read_sample(channel, sample).unwrap(), *expected);
            }
        }
    }

    #[test]
    fn cancellation_leaves_no_partial_destination() {
        let store_dir = tempfile::tempdir().unwrap();
        let capture = capture(store_dir.path());
        let output_dir = tempfile::tempdir().unwrap();
        let output = output_dir.path().join("cancelled.dsl");
        let error = export_finalized_capture(
            &capture,
            &CaptureExportRequest {
                destination: output.clone(),
                format: RawCaptureExportFormat::Dsl,
                overwrite: false,
            },
            &mut CancelImmediately,
        )
        .unwrap_err();
        assert!(matches!(error, CaptureExportError::Cancelled));
        assert!(!output.exists());
    }
}
