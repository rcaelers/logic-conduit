use std::path::PathBuf;
use std::sync::mpsc::Sender;

use signal_processing::CaptureDataSource;

use crate::viewer::LogicAnalyzerViewer;

pub(crate) enum WorkerResponse {
    Unsupported { path: PathBuf },
}

impl LogicAnalyzerViewer {
    pub(crate) fn process_worker_responses(&mut self) {
        let Some(receiver) = &self.worker_responses else {
            return;
        };
        let responses: Vec<_> = receiver.try_iter().collect();
        for WorkerResponse::Unsupported { path } in responses {
            if self.capture_path.as_deref() == Some(path.as_path()) {
                self.capture_path = None;
                self.worker_responses = None;
                self.status = "File-backed captures are unavailable in the web app".to_string();
            }
        }
    }
}

pub(crate) fn spawn_capture_worker(
    identity: PathBuf,
    _data_source: impl CaptureDataSource,
    responses: Sender<WorkerResponse>,
) {
    let _ = responses.send(WorkerResponse::Unsupported { path: identity });
}
