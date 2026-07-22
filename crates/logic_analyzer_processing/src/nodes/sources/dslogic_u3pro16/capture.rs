//! U3Pro16 live-capture setup and preparation.

use std::sync::Arc;

use signal_processing::{
    AcquisitionContext, AcquisitionError, AcquisitionResult, CaptureChannelId, CaptureDataDelivery,
    PreparedAcquisition,
};

use super::buffered::BufferedProvider;
use super::implementation::{DsLogicCapturePlan, LinkSpeed};
use super::streaming::StreamingProvider;
use crate::support::logic_analyzer::{CaptureMode, LogicCaptureConfig};

#[derive(Clone, Copy)]
enum CaptureProfile {
    Buffered,
    Streaming,
}

/// A configured U3Pro16 live capture.
///
/// This is the concrete acquisition counterpart to [`super::DsLogicU3Pro16Source`].
/// It owns all device-profile selection; callers only configure it, obtain its
/// generic capture facts, and prepare it through the acquisition runtime.
#[derive(Clone)]
pub struct DsLogicU3Pro16Capture {
    config: LogicCaptureConfig,
    channels: Arc<[CaptureChannelId]>,
    profile: CaptureProfile,
    capture_window_samples: u64,
}

impl DsLogicU3Pro16Capture {
    /// Validates a U3Pro16 capture request without opening the device.
    pub fn new(
        config: LogicCaptureConfig,
        channels: impl Into<Arc<[CaptureChannelId]>>,
    ) -> AcquisitionResult<Self> {
        let channels = channels.into();
        if channels.is_empty() || channels.len() != config.input_mask.count_ones() as usize {
            return Err(AcquisitionError::InvalidRequest(
                "U3Pro16 channel identities must match the enabled physical inputs".into(),
            ));
        }
        let (profile, capture_window_samples) = match config.mode {
            CaptureMode::Finite => (
                CaptureProfile::Buffered,
                DsLogicCapturePlan::new_buffered(&config)
                    .map_err(|error| AcquisitionError::InvalidRequest(error.to_string()))?
                    .actual_samples(),
            ),
            CaptureMode::Streaming => {
                let high = DsLogicCapturePlan::new_streaming(&config, LinkSpeed::High);
                let super_speed = DsLogicCapturePlan::new_streaming(&config, LinkSpeed::Super);
                if let (Err(high), Err(super_speed)) = (high, super_speed) {
                    return Err(AcquisitionError::InvalidRequest(format!(
                        "U3Pro16 stream is unsupported on High Speed ({high}) and SuperSpeed ({super_speed})"
                    )));
                }
                (CaptureProfile::Streaming, config.sample_limit)
            }
        };
        Ok(Self {
            config,
            channels,
            profile,
            capture_window_samples,
        })
    }

    /// Returns the delivery behavior selected by this capture request.
    pub const fn data_delivery(&self) -> CaptureDataDelivery {
        match self.profile {
            CaptureProfile::Buffered => CaptureDataDelivery::BufferedUpload,
            CaptureProfile::Streaming => CaptureDataDelivery::DuringAcquisition,
        }
    }

    /// Returns the validated capture window size.
    pub const fn capture_window_samples(&self) -> u64 {
        self.capture_window_samples
    }

    /// Clears hardware triggering for a capture-now request.
    pub fn without_trigger(mut self) -> Self {
        self.config.trigger = Default::default();
        self
    }

    /// Opens and prepares the configured device through the generic acquisition runtime.
    pub fn prepare(
        self,
        context: AcquisitionContext,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        match self.profile {
            CaptureProfile::Buffered => {
                BufferedProvider::open_first(self.config, self.channels)?.prepare(context)
            }
            CaptureProfile::Streaming => {
                StreamingProvider::open_first(self.config, self.channels)?.prepare(context)
            }
        }
    }
}
