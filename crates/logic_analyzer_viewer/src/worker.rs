use crate::channel::placeholder_channels;
use crate::types::{CaptureInfo, IndexBuildProgress};
use crate::viewer::LogicAnalyzerViewer;
use dsl::{CaptureDataSource, CaptureIndex, CaptureIndexProgress, CaptureMetadata, IndexSampler};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

pub(crate) enum WorkerResponse {
    Opened {
        path: PathBuf,
        header: CaptureMetadata,
        duration_us: f64,
    },
    Status {
        path: PathBuf,
        message: String,
    },
    IndexProgress {
        path: PathBuf,
        progress: CaptureIndexProgress,
    },
    /// The worker's own sampler, handed straight to the UI thread — no
    /// second open needed, unlike a path-keyed re-open this doesn't need to
    /// know how the source was constructed in the first place.
    IndexReady {
        path: PathBuf,
        sampler: Box<dyn CaptureIndex + Send>,
    },
    Error {
        path: PathBuf,
        message: String,
    },
}

impl LogicAnalyzerViewer {
    pub(crate) fn process_worker_responses(&mut self) {
        let mut responses = Vec::new();
        if let Some(receiver) = &self.worker_responses {
            responses.extend(receiver.try_iter());
        }

        for response in responses {
            match response {
                WorkerResponse::Opened {
                    path,
                    header,
                    duration_us,
                } => {
                    if self.capture_path.as_deref() != Some(path.as_path()) {
                        continue;
                    }
                    self.capture_info = Some(CaptureInfo {
                        path: path.clone(),
                        header: header.clone(),
                        duration_us,
                    });
                    self.visible_start_us = 0.0;
                    self.visible_span_us = duration_us.max(1.0);
                    self.fit_to_capture = true;
                    if let Some(capture) = self.capture_info.as_ref() {
                        self.status = capture_status(capture);
                    }
                    self.ensure_channel_order(header.total_probes.min(16));
                    let mut channels = placeholder_channels(&header);
                    self.apply_channel_names(&mut channels);
                    self.apply_channel_order(&mut channels);
                    self.channels = channels;
                    self.sampler = None;
                    self.sampled_key = None;
                    self.index_progress = None;
                }
                WorkerResponse::Status { path, message } => {
                    if self.capture_path.as_deref() == Some(path.as_path()) {
                        self.status = message;
                    }
                }
                WorkerResponse::IndexProgress { path, progress } => {
                    if self.capture_path.as_deref() == Some(path.as_path()) {
                        self.index_progress = Some(IndexBuildProgress {
                            completed_roots: progress.completed_roots,
                            total_roots: progress.total_roots,
                        });
                        self.status = format!(
                            "Building waveform index… {}/{}",
                            progress.completed_roots, progress.total_roots
                        );
                    }
                }
                WorkerResponse::IndexReady { path, sampler } => {
                    if self.capture_path.as_deref() != Some(path.as_path()) {
                        continue;
                    }
                    self.index_progress = None;
                    self.sampler = Some(sampler);
                    self.sampled_key = None;
                    if self.fit_to_capture {
                        self.fit_capture();
                    }
                    self.status = self
                        .capture_info
                        .as_ref()
                        .map(capture_status)
                        .unwrap_or_else(|| "Capture ready".to_string());
                }
                WorkerResponse::Error { path, message } => {
                    if self.capture_path.as_deref() == Some(path.as_path()) {
                        self.status = message;
                    }
                }
            }
        }
    }
}

/// Opens the capture and builds (or validates) the waveform index on a
/// background thread, reporting progress. Window sampling itself happens
/// synchronously on the UI thread once the index is ready.
pub(crate) fn spawn_capture_worker(
    identity: PathBuf,
    data_source: impl CaptureDataSource,
    responses: Sender<WorkerResponse>,
) {
    std::thread::Builder::new()
        .name("dsl_capture_indexer".to_string())
        .spawn(move || {
            let header = data_source.metadata().clone();
            let duration_us = header.duration_us();
            if responses
                .send(WorkerResponse::Opened {
                    path: identity.clone(),
                    header,
                    duration_us,
                })
                .is_err()
            {
                return;
            }

            if responses
                .send(WorkerResponse::Status {
                    path: identity.clone(),
                    message: "Building waveform index…".to_string(),
                })
                .is_err()
            {
                return;
            }

            let progress_path = identity.clone();
            let progress_responses = responses.clone();
            let mut last_progress_sent = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(100))
                .unwrap_or_else(std::time::Instant::now);
            let mut last_progress_completed = 0_usize;
            let result = IndexSampler::open_data_source_with_progress(data_source, |progress| {
                let now = std::time::Instant::now();
                let is_first = progress.completed_roots == 0;
                let is_done = progress.completed_roots >= progress.total_roots;
                let enough_time =
                    now.duration_since(last_progress_sent) >= std::time::Duration::from_millis(100);
                let enough_work = progress
                    .completed_roots
                    .saturating_sub(last_progress_completed)
                    >= 64;
                if is_first || is_done || enough_time || enough_work {
                    last_progress_sent = now;
                    last_progress_completed = progress.completed_roots;
                    let _ = progress_responses.send(WorkerResponse::IndexProgress {
                        path: progress_path.clone(),
                        progress,
                    });
                }
            });

            let response = match result {
                Ok(sampler) => WorkerResponse::IndexReady {
                    path: identity,
                    sampler: Box::new(sampler),
                },
                Err(err) => WorkerResponse::Error {
                    path: identity,
                    message: format!("Could not open capture: {err}"),
                },
            };
            let _ = responses.send(response);
        })
        .expect("capture indexer thread should start");
}

pub(crate) fn capture_status(capture: &CaptureInfo) -> String {
    format!(
        "{} · {} · {:.1} MHz · {} samples",
        capture
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("capture"),
        capture.header.samplerate,
        capture.header.samplerate_hz / 1_000_000.0,
        capture.header.total_samples
    )
}
