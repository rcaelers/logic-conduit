//! Sigrok v2 (`.sr`) processing-node file source.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;

use signal_processing::{
    CaptureIndex, CaptureIndexBuildProgress, CaptureIndexFactory, InputPort, OutputPort,
    PortDirection, PortSchema, ProcessNode, Result, Sample, Sender, WorkError, WorkResult,
};

use crate::support::sigrok_file::{SigrokCapture, SigrokFileCaptureDataSource};
use crate::support::capture_index::capture_cache_identity;

/// A PulseView/sigrok v2 session source.
pub struct SigrokFileSource {
    name: String,
    capture: SigrokCapture,
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
            self.samples[sample * self.unitsize + self.channel / 8] & (1 << (self.channel % 8)) != 0
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

struct SigrokCaptureIndexFactory {
    path: PathBuf,
}

impl CaptureIndexFactory for SigrokCaptureIndexFactory {
    fn display_name(&self) -> String {
        self.path.display().to_string()
    }

    fn open(
        self: Box<Self>,
        progress: &mut dyn FnMut(CaptureIndexBuildProgress),
    ) -> Result<Box<dyn CaptureIndex + Send>> {
        let source = SigrokFileCaptureDataSource::open(&self.path)?;
        signal_processing::IndexSampler::open_data_source_with_progress(source, |value| {
            progress(CaptureIndexBuildProgress {
                completed: value.completed_roots,
                total: value.total_roots,
            });
        })
        .map(|index| Box::new(index) as Box<dyn CaptureIndex + Send>)
    }
}

impl SigrokFileSource {
    /// Creates the generic indexed-capture presentation for a static sigrok file.
    pub fn indexed_capture_presentation(
        path: impl AsRef<Path>,
    ) -> signal_processing::IndexedCapturePresentation {
        let path = path.as_ref().to_path_buf();
        signal_processing::IndexedCapturePresentation {
            identity: path.clone(),
            factory: Box::new(SigrokCaptureIndexFactory { path }),
        }
    }

    /// Returns the persistent-cache identity for a static sigrok file.
    pub fn capture_cache_identity(path: impl AsRef<Path>) -> Result<[u8; 32]> {
        let path = path.as_ref();
        let source = SigrokFileCaptureDataSource::open(path)?;
        Ok(capture_cache_identity(path, &source))
    }

    pub fn new(path: impl AsRef<Path>, num_channels: u8) -> Result<Self> {
        Ok(Self {
            name: "sigrok_file_source".into(),
            capture: SigrokCapture::open(path, num_channels)?,
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

    pub fn header(&self) -> &signal_processing::CaptureMetadata {
        self.capture.metadata()
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
        let timestamp_step = (1_000_000_000.0 / self.capture.metadata().samplerate_hz) as u64;
        let mut threads = Vec::new();
        for channel in 0..self.num_channels as usize {
            let Some(senders) = outputs
                .get(channel)
                .and_then(|output| output.split_senders::<Sample>())
            else {
                continue;
            };
            for sender in senders {
                let samples = self.capture.samples();
                let shutdown = Arc::clone(&self.shutdown);
                let completed = Arc::clone(&self.completed);
                let unitsize = self.capture.unitsize();
                let total_samples = self.capture.metadata().total_samples as usize;
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use signal_processing::capture::{CaptureDataSource, CaptureSource};

    use super::*;
    use crate::support::sigrok_file::SigrokFileCaptureDataSource;

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
    fn data_source_is_private_support_for_the_node() {
        let dir = fixture();
        let source = SigrokFileCaptureDataSource::open(dir.path().join("hello.sr")).unwrap();
        assert_eq!(source.metadata().total_samples, 8);
        assert_eq!(source.open_reader().unwrap().metadata().total_probes, 8);
    }
}
