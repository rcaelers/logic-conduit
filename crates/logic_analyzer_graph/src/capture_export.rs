//! Application-facing adapter for concrete raw-capture exporters.

use std::path::{Path, PathBuf};

use logic_analyzer_processing::{
    CaptureExportObserver as ProcessingCaptureExportObserver,
    CaptureExportProgress as ProcessingCaptureExportProgress, CaptureExportRequest,
    RawCaptureExportFormat,
};
use signal_processing::NativeFinalizedCapture;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureExportFormat {
    Portable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureExportDescriptor {
    pub label: &'static str,
    pub extension: &'static str,
    pub dialog_title: &'static str,
    pub default_file_name: &'static str,
}

impl CaptureExportFormat {
    pub const fn descriptor(self) -> CaptureExportDescriptor {
        match self {
            Self::Portable => CaptureExportDescriptor {
                label: "PulseView capture",
                extension: "sr",
                dialog_title: "Save Capture Data",
                default_file_name: "capture.sr",
            },
        }
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureExportReport {
    pub destination: PathBuf,
    pub samples_written: u64,
    pub encoded_bytes: u64,
    pub warnings: Vec<String>,
}

struct ObserverAdapter<'a>(&'a mut dyn CaptureExportObserver);

impl ProcessingCaptureExportObserver for ObserverAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    fn on_progress(&mut self, progress: ProcessingCaptureExportProgress) {
        self.0.on_progress(CaptureExportProgress {
            samples_written: progress.samples_written,
            total_samples: progress.total_samples,
        });
    }
}

pub fn export_finalized_capture(
    capture: &NativeFinalizedCapture,
    format: CaptureExportFormat,
    destination: &Path,
    observer: &mut dyn CaptureExportObserver,
) -> Result<CaptureExportReport, String> {
    let request = CaptureExportRequest {
        destination: destination.to_owned(),
        format: match format {
            CaptureExportFormat::Portable => RawCaptureExportFormat::SigrokV2,
        },
        overwrite: true,
    };
    let report = logic_analyzer_processing::export_finalized_capture(
        capture,
        &request,
        &mut ObserverAdapter(observer),
    )
    .map_err(|error| error.to_string())?;
    Ok(CaptureExportReport {
        destination: report.destination,
        samples_written: report.samples_written,
        encoded_bytes: report.encoded_bytes,
        warnings: report
            .warnings
            .into_iter()
            .map(|warning| warning.message().to_owned())
            .collect(),
    })
}
