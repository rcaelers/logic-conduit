//! DSLogic U3Pro16 USB processing-node driver.
//!
//! The wire protocol is kept here, below the generic `LogicAnalyzer` boundary.
//! `RusbTransport` is deliberately small so a libsigrok-backed transport can be
//! added without changing capture packet construction or graph integration.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rusb::{Context, DeviceHandle, UsbContext};

use signal_processing::TriggerCountMode;

use super::logic_analyzer::{
    CaptureMode, ClockEdge, ClockSource, LogicAnalyzer, LogicAnalyzerError, LogicAnalyzerInfo,
    LogicAnalyzerResult, LogicAnalyzerSource, LogicCaptureConfig, LogicChunk, LogicEncoding,
    LogicEncodingRequest, LogicTrigger, LogicTriggerStage, TriggerCondition, TriggerLogic,
};

const VID: u16 = 0x2a0e;
const PID: u16 = 0x002a;
const BULK_OUT: u8 = 0x02;
const BULK_IN: u8 = 0x86;
const CONTROL_TIMEOUT: Duration = Duration::from_millis(3_000);
const BULK_TIMEOUT: Duration = Duration::from_millis(1_000);
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_MANUFACTURER: &str = "DreamSourceLab";
const RUNTIME_PRODUCT: &str = "USB-based DSL Instrument v2";
const RATES: &[u64] = &[
    10,
    20,
    50,
    100,
    200,
    500,
    1_000,
    2_000,
    5_000,
    10_000,
    20_000,
    40_000,
    50_000,
    100_000,
    200_000,
    400_000,
    500_000,
    1_000_000,
    2_000_000,
    4_000_000,
    5_000_000,
    10_000_000,
    20_000_000,
    25_000_000,
    50_000_000,
    100_000_000,
    125_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSpeed {
    High,
    Super,
}

/// Generic user-facing capture settings for this device.
#[derive(Debug, Clone)]
pub struct DsLogicCaptureSettings {
    pub mode: CaptureMode,
    pub sample_rate_hz: u64,
    /// Physical DSLogic input bits. The source exposes enabled inputs in increasing order.
    pub input_mask: u16,
    pub sample_limit: u64,
    pub trigger_percent: u8,
    pub threshold_volts: Option<f32>,
    pub trigger: LogicTrigger,
    pub run_length: bool,
    pub external_clock: bool,
    pub external_clock_active_edge: bool,
    pub input_filter: bool,
}

impl DsLogicCaptureSettings {
    pub fn finite(sample_rate_hz: u64, input_mask: u16, sample_limit: u64) -> Self {
        Self {
            mode: CaptureMode::Finite,
            sample_rate_hz,
            input_mask,
            sample_limit,
            trigger_percent: 50,
            threshold_volts: None,
            trigger: LogicTrigger::default(),
            run_length: false,
            external_clock: false,
            external_clock_active_edge: false,
            input_filter: false,
        }
    }
}

fn settings_from_config(config: &LogicCaptureConfig) -> LogicAnalyzerResult<DsLogicCaptureSettings> {
    if config.input_mask > u64::from(u16::MAX) {
        return Err(LogicAnalyzerError::InvalidSettings(
            "U3Pro16 has only 16 inputs".into(),
        ));
    }
    if config.threshold_volts.is_some_and(|volts| !volts.is_finite()) {
        return Err(LogicAnalyzerError::InvalidSettings(
            "threshold must be finite".into(),
        ));
    }
    if config.trigger_percent > 100 {
        return Err(LogicAnalyzerError::InvalidSettings(
            "trigger position percentage must be within 0..=100".into(),
        ));
    }
    if config.trigger.stages.len() > 16 {
        return Err(LogicAnalyzerError::InvalidSettings(
            "U3Pro16 supports at most 16 trigger stages".into(),
        ));
    }
    let mut settings = DsLogicCaptureSettings::finite(
        config.sample_rate_hz,
        config.input_mask as u16,
        config.sample_limit,
    );
    settings.mode = config.mode;
    settings.trigger_percent = config.trigger_percent;
    settings.threshold_volts = config.threshold_volts;
    settings.trigger = config.trigger.clone();
    settings.run_length = config.encoding == LogicEncodingRequest::RunLength;
    settings.external_clock = matches!(config.clock, ClockSource::External { .. });
    settings.external_clock_active_edge = matches!(
        config.clock,
        ClockSource::External {
            edge: ClockEdge::Rising
        }
    );
    settings.input_filter = config.input_filter;
    Ok(settings)
}

/// Validates a concrete finite request without opening hardware. Finite-mode
/// memory/rate constraints are identical at high- and SuperSpeed.
pub fn u3pro16_buffered_plan(
    config: &LogicCaptureConfig,
) -> LogicAnalyzerResult<DsLogicCapturePlan> {
    if config.mode != CaptureMode::Finite {
        return Err(LogicAnalyzerError::InvalidSettings(
            "buffered acquisition requires finite capture mode".into(),
        ));
    }
    build_plan(LinkSpeed::Super, &settings_from_config(config)?)
}

/// Validates a host-streamed request against one concrete USB link speed.
pub fn u3pro16_streaming_plan(
    config: &LogicCaptureConfig,
    link_speed: LinkSpeed,
) -> LogicAnalyzerResult<DsLogicCapturePlan> {
    if config.mode != CaptureMode::Streaming {
        return Err(LogicAnalyzerError::InvalidSettings(
            "host-streamed acquisition requires streaming capture mode".into(),
        ));
    }
    build_plan(link_speed, &settings_from_config(config)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbError {
    Timeout,
    Other,
}

/// USB operations required by the protocol. Implementations must preserve call order.
pub trait UsbTransport: Send + 'static {
    fn link_speed(&self) -> LinkSpeed;
    fn control_write(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, UsbError>;
    fn control_read(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, UsbError>;
    fn bulk_write(
        &mut self,
        endpoint: u8,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, UsbError>;
    fn bulk_read(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, UsbError>;
    fn close(&mut self) -> Result<(), UsbError> {
        Ok(())
    }
}

/// Production `rusb` transport. It claims interface 0 during discovery.
pub struct RusbTransport {
    handle: DeviceHandle<Context>,
    speed: LinkSpeed,
    claimed: bool,
}

impl RusbTransport {
    pub fn open_first() -> LogicAnalyzerResult<Self> {
        let context = Context::new().map_err(rusb_error)?;
        let devices = context.devices().map_err(rusb_error)?;
        for device in devices.iter() {
            let descriptor = device.device_descriptor().map_err(rusb_error)?;
            if descriptor.vendor_id() != VID || descriptor.product_id() != PID {
                continue;
            }
            let speed = match device.speed() {
                rusb::Speed::High => LinkSpeed::High,
                rusb::Speed::Super => LinkSpeed::Super,
                _ => continue,
            };
            let handle = device.open().map_err(rusb_error)?;
            let manufacturer = handle
                .read_manufacturer_string_ascii(&descriptor)
                .map_err(rusb_error)?;
            let product = handle
                .read_product_string_ascii(&descriptor)
                .map_err(rusb_error)?;
            if !manufacturer.starts_with(RUNTIME_MANUFACTURER)
                || !product.starts_with(RUNTIME_PRODUCT)
            {
                continue;
            }
            if handle.active_configuration().map_err(rusb_error)? != 1 {
                handle.set_active_configuration(1).map_err(rusb_error)?;
            }
            if handle.kernel_driver_active(0).unwrap_or(false) {
                let _ = handle.detach_kernel_driver(0);
            }
            handle.claim_interface(0).map_err(rusb_error)?;
            return Ok(Self {
                handle,
                speed,
                claimed: true,
            });
        }
        Err(LogicAnalyzerError::Transport(
            "no accessible DSLogic U3Pro16 runtime device found".into(),
        ))
    }

    /// Open a device in FX2 boot mode for the explicit recovery API. Runtime
    /// strings are deliberately not accepted here; callers must supply the
    /// exact U3Pro16 firmware image to `recover_usb_firmware`.
    pub fn open_bootloader() -> LogicAnalyzerResult<Self> {
        let context = Context::new().map_err(rusb_error)?;
        let devices = context.devices().map_err(rusb_error)?;
        for device in devices.iter() {
            let descriptor = device.device_descriptor().map_err(rusb_error)?;
            if descriptor.vendor_id() != VID || descriptor.product_id() != PID {
                continue;
            }
            let speed = match device.speed() {
                rusb::Speed::High => LinkSpeed::High,
                rusb::Speed::Super => LinkSpeed::Super,
                _ => continue,
            };
            let handle = device.open().map_err(rusb_error)?;
            if handle.active_configuration().map_err(rusb_error)? != 1 {
                handle.set_active_configuration(1).map_err(rusb_error)?;
            }
            if handle.kernel_driver_active(0).unwrap_or(false) {
                let _ = handle.detach_kernel_driver(0);
            }
            handle.claim_interface(0).map_err(rusb_error)?;
            return Ok(Self {
                handle,
                speed,
                claimed: true,
            });
        }
        Err(LogicAnalyzerError::Transport(
            "no accessible DSLogic FX2 boot-mode device found".into(),
        ))
    }
}

impl UsbTransport for RusbTransport {
    fn link_speed(&self) -> LinkSpeed {
        self.speed
    }
    fn control_write(
        &mut self,
        ty: u8,
        req: u8,
        value: u16,
        index: u16,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, UsbError> {
        self.handle
            .write_control(ty, req, value, index, data, timeout)
            .map_err(map_usb_error)
    }
    fn control_read(
        &mut self,
        ty: u8,
        req: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, UsbError> {
        self.handle
            .read_control(ty, req, value, index, data, timeout)
            .map_err(map_usb_error)
    }
    fn bulk_write(
        &mut self,
        endpoint: u8,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, UsbError> {
        self.handle
            .write_bulk(endpoint, data, timeout)
            .map_err(map_usb_error)
    }
    fn bulk_read(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, UsbError> {
        self.handle
            .read_bulk(endpoint, data, timeout)
            .map_err(map_usb_error)
    }
    fn close(&mut self) -> Result<(), UsbError> {
        if self.claimed {
            self.handle.release_interface(0).map_err(map_usb_error)?;
            self.claimed = false;
        }
        Ok(())
    }
}

fn map_usb_error(error: rusb::Error) -> UsbError {
    if error == rusb::Error::Timeout {
        UsbError::Timeout
    } else {
        UsbError::Other
    }
}
fn rusb_error(error: rusb::Error) -> LogicAnalyzerError {
    LogicAnalyzerError::Transport(error.to_string())
}
fn usb<T>(result: Result<T, UsbError>, action: &str) -> LogicAnalyzerResult<T> {
    result.map_err(|e| match e {
        UsbError::Timeout => LogicAnalyzerError::Timeout(action.into()),
        UsbError::Other => LogicAnalyzerError::Transport(action.into()),
    })
}

pub struct DsLogicU3Pro16<T: UsbTransport = RusbTransport> {
    transport: T,
    info: LogicAnalyzerInfo,
    settings: DsLogicCaptureSettings,
    plan: Option<DsLogicCapturePlan>,
    trigger_header: Option<DsLogicTriggerHeader>,
    prepared: bool,
    active: bool,
    header_pending: bool,
    bytes_remaining: Option<usize>,
    bit_position: u64,
}

/// Immutable, validated device-buffered acquisition plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DsLogicCapturePlan {
    channels: u8,
    actual_samples: u64,
    actual_bytes: usize,
    stream_buffer: usize,
}

impl DsLogicCapturePlan {
    pub const fn channel_count(self) -> u8 {
        self.channels
    }

    pub const fn actual_samples(self) -> u64 {
        self.actual_samples
    }

    pub const fn actual_bytes(self) -> usize {
        self.actual_bytes
    }

    pub const fn stream_buffer_bytes(self) -> usize {
        self.stream_buffer
    }
}

/// Device header translated into the capture timeline used by the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DsLogicTriggerHeader {
    trigger_sample: Option<u64>,
    captured_samples: u64,
    remaining_samples: u64,
    ram_start: u32,
}

impl DsLogicTriggerHeader {
    pub const fn trigger_sample(self) -> Option<u64> {
        self.trigger_sample
    }

    pub const fn captured_samples(self) -> u64 {
        self.captured_samples
    }

    pub const fn remaining_samples(self) -> u64 {
        self.remaining_samples
    }

    pub const fn ram_start(self) -> u32 {
        self.ram_start
    }
}

impl DsLogicU3Pro16<RusbTransport> {
    pub fn open_first() -> LogicAnalyzerResult<Self> {
        Self::new(RusbTransport::open_first()?)
    }

    /// Open boot mode for explicit firmware recovery. Drop this instance and
    /// rediscover with `open_first` after the device re-enumerates.
    pub fn open_bootloader() -> LogicAnalyzerResult<Self> {
        Self::new(RusbTransport::open_bootloader()?)
    }
}

impl<T: UsbTransport> DsLogicU3Pro16<T> {
    pub fn new(transport: T) -> LogicAnalyzerResult<Self> {
        let settings = DsLogicCaptureSettings::finite(1_000_000, 1, 1024);
        let info = LogicAnalyzerInfo {
            driver: "dslogic_u3pro16".into(),
            model: "DSLogic U3Pro16".into(),
            channels: 16,
            sample_rates_hz: RATES.to_vec(),
        };
        Ok(Self {
            transport,
            info,
            settings,
            plan: None,
            trigger_header: None,
            prepared: false,
            active: false,
            header_pending: false,
            bytes_remaining: None,
            bit_position: 0,
        })
    }

    /// Configure the capture FPGA with the exact U3Pro16 `.bin` image.
    pub fn configure_fpga(&mut self, image: &[u8]) -> LogicAnalyzerResult<()> {
        if image.is_empty() || image.len() > 0x00ff_ffff {
            return Err(LogicAnalyzerError::InvalidSettings(
                "FPGA image must be 1..=0x00ffffff bytes".into(),
            ));
        }
        self.command_write(3, 0, &[0xfb])?;
        self.command_write(5, 0, &[0xfc])?;
        self.command_write(3, 0, &[0x04])?;
        self.poll_status(0x20)?;
        self.command_write(6, 0, &[0x7f])?;
        self.command_write(
            10,
            0,
            &[
                (image.len() & 0xff) as u8,
                ((image.len() >> 8) & 0xff) as u8,
                ((image.len() >> 16) & 0xff) as u8,
            ],
        )?;
        self.bulk_write_all(image, BULK_TIMEOUT)?;
        self.command_write(6, 0, &[0x80])?;
        self.poll_status(0x80)?;
        self.command_write(6, 0, &[0x7f])?;
        // Some U3Pro16 firmware revisions leave the configured-status bit
        // clear after a successful upload. The logic-version register is the
        // authoritative compatibility check in that case.
        if let Err(status_error) = self.poll_status_for(0x40, STATUS_TIMEOUT) {
            let logic_version = self.command_read_byte(15, 0x04)?;
            if logic_version != 0x0e {
                return Err(LogicAnalyzerError::Protocol(format!(
                    "FPGA did not configure (logic version {logic_version:#04x}): {status_error}"
                )));
            }
            tracing::warn!(%status_error, "FPGA configured despite a clear configured-status bit");
        }
        self.command_write(5, 0, &[0x01])?;
        self.command_write(7, 0, &[0x01])?;
        Ok(())
    }

    /// Move this configured driver into a graph source node.
    pub fn into_source(
        self,
        config: LogicCaptureConfig,
    ) -> LogicAnalyzerResult<LogicAnalyzerSource<Self>> {
        LogicAnalyzerSource::new(self, config)
    }

    /// Validates a finite buffered request against the connected device and
    /// freezes the plan used by the subsequent `start_capture` call.
    pub fn negotiate_buffered_capture(
        &mut self,
        config: &LogicCaptureConfig,
    ) -> LogicAnalyzerResult<DsLogicCapturePlan> {
        if config.mode != CaptureMode::Finite {
            return Err(LogicAnalyzerError::InvalidSettings(
                "buffered acquisition requires finite capture mode".into(),
            ));
        }
        self.configure_capture(config)?;
        let plan = self.plan()?;
        self.plan = Some(plan);
        Ok(plan)
    }

    /// Negotiates and configures the device without arming acquisition.
    /// `start_capture` then performs only the final Start command.
    pub fn prepare_buffered_capture(
        &mut self,
        config: &LogicCaptureConfig,
    ) -> LogicAnalyzerResult<DsLogicCapturePlan> {
        let plan = self.negotiate_buffered_capture(config)?;
        self.configure_device_capture(plan)?;
        Ok(plan)
    }

    /// Validates a host-streamed request against the connected link and
    /// configures the device without issuing the final Start command.
    pub fn prepare_streaming_capture(
        &mut self,
        config: &LogicCaptureConfig,
    ) -> LogicAnalyzerResult<DsLogicCapturePlan> {
        if config.mode != CaptureMode::Streaming {
            return Err(LogicAnalyzerError::InvalidSettings(
                "host-streamed acquisition requires streaming capture mode".into(),
            ));
        }
        self.configure_capture(config)?;
        let plan = self.plan()?;
        self.plan = Some(plan);
        self.configure_device_capture(plan)?;
        Ok(plan)
    }

    pub fn take_trigger_header(&mut self) -> Option<DsLogicTriggerHeader> {
        self.trigger_header.take()
    }

    /// Explicitly dangerous FX2 recovery. Only pass the exact U3Pro16 `.fw` image.
    pub fn recover_usb_firmware(&mut self, image: &[u8]) -> LogicAnalyzerResult<()> {
        if image.is_empty() || image.len() > 0x1_0000 {
            return Err(LogicAnalyzerError::InvalidSettings(
                "firmware image must fit in the 16-bit FX2 address space".into(),
            ));
        }
        self.raw_control_write(0x40, 0xa0, 0xe600, &[1])?;
        for (chunk_index, chunk) in image.chunks(4096).enumerate() {
            self.raw_control_write(0x40, 0xa0, (chunk_index * 4096) as u16, chunk)?;
        }
        self.raw_control_write(0x40, 0xa0, 0xe600, &[0])
    }

    /// Explicitly dangerous nonvolatile-memory write; normal captures never call this.
    pub fn dangerous_write_nvm(&mut self, offset: u16, data: &[u8]) -> LogicAnalyzerResult<()> {
        self.command_write(12, offset, data)
    }

    fn raw_control_write(
        &mut self,
        ty: u8,
        request: u8,
        value: u16,
        data: &[u8],
    ) -> LogicAnalyzerResult<()> {
        let len = usb(
            self.transport
                .control_write(ty, request, value, 0, data, CONTROL_TIMEOUT),
            "USB control write",
        )?;
        if len != data.len() {
            return Err(LogicAnalyzerError::Protocol(format!(
                "short control write: {len}/{} bytes",
                data.len()
            )));
        }
        Ok(())
    }
    fn command_write(&mut self, command: u8, offset: u16, data: &[u8]) -> LogicAnalyzerResult<()> {
        if data.len() > 60 {
            return Err(LogicAnalyzerError::InvalidSettings(
                "runtime control writes are limited to 60 data bytes".into(),
            ));
        }
        let mut payload = Vec::with_capacity(4 + data.len());
        payload.push(command);
        payload.extend_from_slice(&offset.to_le_bytes());
        payload.push(data.len() as u8);
        payload.extend_from_slice(data);
        self.raw_control_write(0x40, 0xb0, 0, &payload)
    }
    fn command_read(
        &mut self,
        command: u8,
        offset: u16,
        length: usize,
    ) -> LogicAnalyzerResult<Vec<u8>> {
        if length > u8::MAX as usize {
            return Err(LogicAnalyzerError::InvalidSettings(
                "runtime control read is limited to 255 bytes".into(),
            ));
        }
        let header = [command, offset as u8, (offset >> 8) as u8, length as u8];
        self.raw_control_write(0x40, 0xb1, 0, &header)?;
        thread::sleep(Duration::from_millis(10));
        let mut data = vec![0; length];
        let read = usb(
            self.transport
                .control_read(0xc0, 0xb2, 0, 0, &mut data, CONTROL_TIMEOUT),
            "USB control read",
        )?;
        if read != length {
            return Err(LogicAnalyzerError::Protocol(format!(
                "short control read: {read}/{length} bytes"
            )));
        }
        Ok(data)
    }
    fn command_read_byte(&mut self, command: u8, offset: u16) -> LogicAnalyzerResult<u8> {
        Ok(self.command_read(command, offset, 1)?[0])
    }
    fn poll_status(&mut self, required: u8) -> LogicAnalyzerResult<u8> {
        self.poll_status_for(required, STATUS_TIMEOUT)
    }
    fn poll_status_for(&mut self, required: u8, timeout: Duration) -> LogicAnalyzerResult<u8> {
        let until = Instant::now() + timeout;
        let mut status = 0;
        while Instant::now() < until {
            status = self.command_read_byte(2, 0)?;
            if status & required == required {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(LogicAnalyzerError::Timeout(format!(
            "status bit(s) {required:#04x}; final status {status:#04x}"
        )))
    }
    fn bulk_write_all(&mut self, mut data: &[u8], timeout: Duration) -> LogicAnalyzerResult<()> {
        while !data.is_empty() {
            let written = usb(
                self.transport.bulk_write(BULK_OUT, data, timeout),
                "USB bulk write",
            )?;
            if written == 0 {
                return Err(LogicAnalyzerError::Protocol(
                    "zero-length bulk write".into(),
                ));
            }
            data = &data[written..];
        }
        Ok(())
    }

    /// Ensure the capture FPGA is ready without exposing image management to
    /// callers. A normal runtime device is already configured. After power-up
    /// or a reset, the driver looks for the exact image in the environment or
    /// well-known local locations.
    fn ensure_fpga_configured(&mut self) -> LogicAnalyzerResult<()> {
        let status = self.command_read_byte(2, 0)?;
        if status & 0x40 != 0 && self.command_read_byte(15, 0x04)? == 0x0e {
            return Ok(());
        }

        let mut candidates = vec![
            std::path::PathBuf::from("DSLogicU3Pro16.bin"),
            std::path::PathBuf::from("firmware/DSLogicU3Pro16.bin"),
            std::path::PathBuf::from(
                "/Applications/DSView.app/Contents/MacOS/res/DSLogicU3Pro16.bin",
            ),
            std::path::PathBuf::from(
                "/Applications/DSView.app/Contents/Resources/driver/DSLogicU3Pro16.bin",
            ),
            std::path::PathBuf::from("/usr/share/DSView/driver/DSLogicU3Pro16.bin"),
            std::path::PathBuf::from("/usr/local/share/DSView/driver/DSLogicU3Pro16.bin"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            let home = std::path::PathBuf::from(home);
            candidates.push(home.join(".local/share/DSView/driver/DSLogicU3Pro16.bin"));
            candidates
                .push(home.join("Library/Application Support/DSView/driver/DSLogicU3Pro16.bin"));
        }
        // Explicit environment configuration is useful for non-standard
        // installs, but is intentionally tried only after normal locations.
        if let Some(path) = std::env::var_os("DSLOGIC_U3PRO16_FPGA_IMAGE") {
            candidates.push(std::path::PathBuf::from(path));
        }

        for path in candidates {
            if !path.is_file() {
                continue;
            }
            let image = std::fs::read(&path).map_err(|error| {
                LogicAnalyzerError::Transport(format!(
                    "cannot read U3Pro16 FPGA image '{}': {error}",
                    path.display()
                ))
            })?;
            tracing::info!(path = %path.display(), "configuring DSLogic U3Pro16 FPGA");
            return self.configure_fpga(&image);
        }

        Err(LogicAnalyzerError::Protocol(
            "the U3Pro16 FPGA is absent or has an incompatible image, and DSLogicU3Pro16.bin was not found; set DSLOGIC_U3PRO16_FPGA_IMAGE to the exact image".into(),
        ))
    }
    fn plan(&self) -> LogicAnalyzerResult<DsLogicCapturePlan> {
        build_plan(self.transport.link_speed(), &self.settings)
    }
    fn settings_packet(&self, plan: DsLogicCapturePlan) -> LogicAnalyzerResult<[u8; 672]> {
        build_settings_packet(self.transport.link_speed(), &self.settings, plan)
    }

    fn configure_device_capture(
        &mut self,
        plan: DsLogicCapturePlan,
    ) -> LogicAnalyzerResult<()> {
        self.ensure_fpga_configured()?;
        if self.command_read_byte(15, 0x04)? != 0x0e {
            return Err(LogicAnalyzerError::Protocol(
                "unexpected FPGA logic version".into(),
            ));
        }
        let firmware = self.command_read(0, 0, 2)?;
        if firmware[0] != 2 {
            return Err(LogicAnalyzerError::Protocol(format!(
                "unsupported runtime firmware major version {}",
                firmware[0]
            )));
        }
        if let Some(volts) = self.settings.threshold_volts {
            if !volts.is_finite() {
                return Err(LogicAnalyzerError::InvalidSettings(
                    "threshold must be finite".into(),
                ));
            }
            let code = (volts.clamp(0.0, 3.3) / 3.3 * 1.5 / 2.5 * 255.0).floor() as u8;
            self.command_write(14, 0x78, &[code])?;
        }
        let packet = self.settings_packet(plan)?;
        self.command_write(9, 0, &[])?;
        if self.transport.link_speed() == LinkSpeed::High {
            self.command_write(7, 0, &[1])?;
        }
        // Command 10 receives the packet length in 16-bit words for this protocol path.
        self.command_write(10, 0, &[0x50, 0x01, 0])?;
        self.poll_status(0x08)?;
        self.bulk_write_all(&packet, BULK_TIMEOUT)?;
        self.command_write(6, 0, &[0x80])?;
        self.poll_status(0x80)?;
        self.prepared = true;
        Ok(())
    }

    fn check_streaming_integrity(&mut self) -> LogicAnalyzerResult<()> {
        let status = self.command_read_byte(2, 0)?;
        if status & 0x10 != 0 {
            return Err(LogicAnalyzerError::Integrity(
                "U3Pro16 reported a streaming overflow".into(),
            ));
        }
        Ok(())
    }
}

impl<T: UsbTransport> LogicAnalyzer for DsLogicU3Pro16<T> {
    fn info(&self) -> &LogicAnalyzerInfo {
        &self.info
    }
    fn configure_capture(&mut self, config: &LogicCaptureConfig) -> LogicAnalyzerResult<()> {
        self.settings = settings_from_config(config)?;
        self.plan = None;
        self.trigger_header = None;
        self.prepared = false;
        Ok(())
    }
    fn sample_rate_hz(&self) -> u64 {
        self.settings.sample_rate_hz
    }
    fn start_capture(&mut self) -> LogicAnalyzerResult<()> {
        if self.active {
            return Err(LogicAnalyzerError::Protocol(
                "capture already active".into(),
            ));
        }
        let plan = match self.plan {
            Some(plan) => plan,
            None => self.plan()?,
        };
        if !self.prepared {
            self.configure_device_capture(plan)?;
        }
        self.command_write(8, 0, &[])?;
        self.plan = Some(plan);
        self.active = true;
        self.prepared = false;
        self.header_pending = true;
        self.bytes_remaining = None;
        self.bit_position = 0;
        self.trigger_header = None;
        Ok(())
    }
    fn next_chunk(&mut self) -> LogicAnalyzerResult<Option<LogicChunk>> {
        if !self.active {
            return Err(LogicAnalyzerError::NotCapturing);
        }
        let plan = self.plan.ok_or(LogicAnalyzerError::NotCapturing)?;
        if self.header_pending {
            let mut header = [0u8; 1024];
            let read = match self.transport.bulk_read(BULK_IN, &mut header, BULK_TIMEOUT) {
                Ok(read) => read,
                Err(UsbError::Timeout) => return Ok(Some(self.empty_chunk(plan))),
                Err(UsbError::Other) => {
                    return Err(LogicAnalyzerError::Transport(
                        "trigger header read".into(),
                    ));
                }
            };
            if read != header.len() {
                return Err(LogicAnalyzerError::Protocol(format!(
                    "short trigger header: {read}/1024 bytes"
                )));
            }
            let translated = translate_trigger_header(&header, self.settings.mode, plan)?;
            if self.settings.mode == CaptureMode::Finite {
                let delivered_bytes = translated
                    .captured_samples
                    .checked_div(64)
                    .and_then(|n| n.checked_mul(u64::from(plan.channels)))
                    .and_then(|n| n.checked_mul(8))
                    .ok_or_else(|| {
                        LogicAnalyzerError::Protocol("capture byte count overflow".into())
                    })?;
                if delivered_bytes > plan.actual_bytes as u64 {
                    return Err(LogicAnalyzerError::Protocol(
                        "trigger header requests more data than the capture buffer".into(),
                    ));
                }
                self.bytes_remaining = Some(usize::try_from(delivered_bytes).map_err(|_| {
                    LogicAnalyzerError::Protocol("capture is too large for this host".into())
                })?);
            }
            self.trigger_header = Some(translated);
            self.header_pending = false;
        }
        if let Some(0) = self.bytes_remaining {
            self.active = false;
            return Ok(None);
        }
        let buffer_len = match self.bytes_remaining {
            Some(left) => left.min(1_048_576),
            None => plan.stream_buffer,
        };
        let mut data = vec![0; buffer_len];
        let timeout = if self.settings.mode == CaptureMode::Finite {
            Duration::from_millis(20)
        } else {
            Duration::from_millis(125)
        };
        let read = match self.transport.bulk_read(BULK_IN, &mut data, timeout) {
            Ok(read) => read,
            Err(UsbError::Timeout) if self.settings.mode == CaptureMode::Streaming => {
                self.check_streaming_integrity()?;
                return Ok(Some(self.empty_chunk(plan)));
            }
            Err(error) => {
                return Err(match error {
                    UsbError::Timeout => LogicAnalyzerError::Timeout("logic data read".into()),
                    UsbError::Other => LogicAnalyzerError::Transport("logic data read".into()),
                });
            }
        };
        if read == 0 {
            if self.settings.mode == CaptureMode::Streaming {
                self.check_streaming_integrity()?;
                return Ok(Some(self.empty_chunk(plan)));
            }
            return Ok(None);
        }
        data.truncate(read);
        if let Some(left) = self.bytes_remaining.as_mut() {
            *left = left.checked_sub(read).ok_or_else(|| {
                LogicAnalyzerError::Protocol("received more than requested capture data".into())
            })?;
        }
        let chunk = LogicChunk {
            data: Arc::from(data),
            bit_offset: 0,
            bit_len: read * 8,
            channel_count: plan.channels,
            start_bit: self.bit_position,
            // Hardware RLE changes device-memory retention. The FPGA expands
            // the upload back into ordinary interleaved samples.
            encoding: LogicEncoding::InterleavedLsbFirst,
        };
        self.bit_position = self
            .bit_position
            .checked_add(chunk.bit_len as u64)
            .ok_or_else(|| LogicAnalyzerError::Protocol("logic bit position overflow".into()))?;
        Ok(Some(chunk))
    }
    fn stop_capture(&mut self) -> LogicAnalyzerResult<()> {
        if self.active {
            self.command_write(14, 0x70, &[0x02])?;
            self.command_write(9, 0, &[])?;
            self.active = false;
        }
        Ok(())
    }
}

impl<T: UsbTransport> DsLogicU3Pro16<T> {
    fn empty_chunk(&self, plan: DsLogicCapturePlan) -> LogicChunk {
        LogicChunk {
            data: Arc::from([]),
            bit_offset: 0,
            bit_len: 0,
            channel_count: plan.channels,
            start_bit: self.bit_position,
            encoding: LogicEncoding::InterleavedLsbFirst,
        }
    }
}

impl<T: UsbTransport> Drop for DsLogicU3Pro16<T> {
    fn drop(&mut self) {
        let _ = self.stop_capture();
        let _ = self.transport.close();
    }
}

fn translate_trigger_header(
    bytes: &[u8],
    mode: CaptureMode,
    plan: DsLogicCapturePlan,
) -> LogicAnalyzerResult<DsLogicTriggerHeader> {
    if bytes.len() < 24 {
        return Err(LogicAnalyzerError::Protocol(format!(
            "short trigger header: {}/24 bytes",
            bytes.len()
        )));
    }
    if u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != 0x5555_5555 {
        return Err(LogicAnalyzerError::Protocol(
            "invalid trigger-header magic".into(),
        ));
    }
    let real_position = u64::from(u32::from_le_bytes(bytes[4..8].try_into().unwrap()));
    let ram_start = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let remaining_samples = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as u64
        | ((u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as u64) << 32);
    let status = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let captured_samples = if mode == CaptureMode::Finite {
        if remaining_samples >= plan.actual_samples {
            return Err(LogicAnalyzerError::Protocol(
                "trigger header remaining count is outside capture limit".into(),
            ));
        }
        (plan.actual_samples - remaining_samples) & !1023
    } else {
        plan.actual_samples
    };
    let trigger_sample = if status & 1 != 0 {
        if real_position >= captured_samples {
            return Err(LogicAnalyzerError::Protocol(format!(
                "trigger sample {real_position} is outside captured extent {captured_samples}"
            )));
        }
        Some(real_position)
    } else {
        None
    };
    Ok(DsLogicTriggerHeader {
        trigger_sample,
        captured_samples,
        remaining_samples,
        ram_start,
    })
}

fn build_plan(
    speed: LinkSpeed,
    settings: &DsLogicCaptureSettings,
) -> LogicAnalyzerResult<DsLogicCapturePlan> {
    if !RATES.contains(&settings.sample_rate_hz) {
        return Err(LogicAnalyzerError::InvalidSettings(
            "sample rate must be one of the device's discrete supported rates".into(),
        ));
    }
    let channels = settings.input_mask.count_ones() as u8;
    if channels == 0 || settings.sample_limit == 0 {
        return Err(LogicAnalyzerError::InvalidSettings(
            "capture requires enabled inputs and a non-zero sample limit".into(),
        ));
    }
    let width = 16 - settings.input_mask.leading_zeros() as u8;
    let max = mode_max_rate(speed, settings.mode, width).ok_or_else(|| {
        LogicAnalyzerError::InvalidSettings(
            "input mask does not fit a hardware acquisition mode".into(),
        )
    })?;
    let min = mode_min_rate(speed, settings.mode, width).unwrap();
    if settings.sample_rate_hz < min || settings.sample_rate_hz > max {
        return Err(LogicAnalyzerError::InvalidSettings(format!(
            "{} Hz is outside this mode's {}..={} Hz range",
            settings.sample_rate_hz, min, max
        )));
    }
    let actual_samples = match settings.mode {
        CaptureMode::Finite => round_up(settings.sample_limit, 1024)?,
        CaptureMode::Streaming => settings.sample_limit,
    };
    let per_input_depth = (2u64 * 1024 * 1024 * 1024 / u64::from(channels)) & !1023;
    if settings.mode == CaptureMode::Finite && actual_samples > per_input_depth {
        return Err(LogicAnalyzerError::InvalidSettings(format!(
            "finite capture requests {actual_samples} samples, but {per_input_depth} are available per enabled input"
        )));
    }
    let actual_bytes = actual_samples
        .checked_mul(u64::from(channels))
        .and_then(|bits| bits.checked_add(7))
        .map(|bits| bits / 8)
        .ok_or_else(|| LogicAnalyzerError::InvalidSettings("capture size overflows".into()))?;
    let bytes_per_ms = div_ceil(
        settings
            .sample_rate_hz
            .checked_mul(u64::from(channels))
            .ok_or_else(|| {
                LogicAnalyzerError::InvalidSettings("rate calculation overflow".into())
            })?,
        8_000,
    )?;
    let stream_buffer = match speed {
        LinkSpeed::Super => round_up_usize(
            usize::try_from(bytes_per_ms.checked_mul(10).ok_or_else(|| {
                LogicAnalyzerError::InvalidSettings("stream buffer overflow".into())
            })?)
            .map_err(|_| {
                LogicAnalyzerError::InvalidSettings("stream buffer is too large".into())
            })?,
            1024,
        ),
        LinkSpeed::High => round_up_usize(
            usize::try_from(bytes_per_ms.checked_mul(20).ok_or_else(|| {
                LogicAnalyzerError::InvalidSettings("stream buffer overflow".into())
            })?)
            .map_err(|_| {
                LogicAnalyzerError::InvalidSettings("stream buffer is too large".into())
            })?,
            512,
        ),
    };
    Ok(DsLogicCapturePlan {
        channels,
        actual_samples,
        actual_bytes: usize::try_from(actual_bytes).map_err(|_| {
            LogicAnalyzerError::InvalidSettings("capture is too large for this host".into())
        })?,
        stream_buffer,
    })
}

fn mode_max_rate(speed: LinkSpeed, mode: CaptureMode, width: u8) -> Option<u64> {
    match (speed, mode) {
        (_, CaptureMode::Finite) if width <= 8 => Some(1_000_000_000),
        (_, CaptureMode::Finite) if width <= 16 => Some(500_000_000),
        (LinkSpeed::High, CaptureMode::Streaming) if width <= 3 => Some(100_000_000),
        (LinkSpeed::High, CaptureMode::Streaming) if width <= 6 => Some(50_000_000),
        (LinkSpeed::High, CaptureMode::Streaming) if width <= 12 => Some(25_000_000),
        (LinkSpeed::High, CaptureMode::Streaming) if width <= 16 => Some(20_000_000),
        (LinkSpeed::Super, CaptureMode::Streaming) if width <= 3 => Some(1_000_000_000),
        (LinkSpeed::Super, CaptureMode::Streaming) if width <= 6 => Some(500_000_000),
        (LinkSpeed::Super, CaptureMode::Streaming) if width <= 12 => Some(250_000_000),
        (LinkSpeed::Super, CaptureMode::Streaming) if width <= 16 => Some(125_000_000),
        _ => None,
    }
}
fn mode_min_rate(speed: LinkSpeed, mode: CaptureMode, _width: u8) -> Option<u64> {
    match (speed, mode) {
        (_, CaptureMode::Finite) => Some(1_000_000),
        (LinkSpeed::High, CaptureMode::Streaming) => Some(100_000),
        (LinkSpeed::Super, CaptureMode::Streaming) => Some(1_000_000),
    }
}
fn div_ceil(n: u64, d: u64) -> LogicAnalyzerResult<u64> {
    n.checked_add(d - 1)
        .map(|v| v / d)
        .ok_or_else(|| LogicAnalyzerError::InvalidSettings("arithmetic overflow".into()))
}
fn round_up(value: u64, multiple: u64) -> LogicAnalyzerResult<u64> {
    div_ceil(value, multiple)?
        .checked_mul(multiple)
        .ok_or_else(|| LogicAnalyzerError::InvalidSettings("arithmetic overflow".into()))
}
fn round_up_usize(value: usize, multiple: usize) -> usize {
    value.div_ceil(multiple) * multiple
}

fn build_settings_packet(
    speed: LinkSpeed,
    settings: &DsLogicCaptureSettings,
    plan: DsLogicCapturePlan,
) -> LogicAnalyzerResult<[u8; 672]> {
    let width = 16 - settings.input_mask.leading_zeros() as u8;
    let max_rate = mode_max_rate(speed, settings.mode, width).unwrap();
    let d0 = div_ceil(max_rate, settings.sample_rate_hz)?;
    let mut divider_high = if d0 >= 5 { 4 << 8 } else { 0 };
    let d = div_ceil(d0, 5)?;
    divider_high |= (d >> 16) as u16;
    let per_input_depth = (2u64 * 1024 * 1024 * 1024 / u64::from(plan.channels)) & !1023;
    let fraction = if settings.mode == CaptureMode::Streaming {
        10
    } else {
        90
    };
    let mut trigger_position = settings
        .sample_limit
        .saturating_mul(u64::from(settings.trigger_percent))
        / 100;
    trigger_position = trigger_position
        .max(64)
        .min(per_input_depth * fraction / 100)
        & !63;
    let mut mode = 0u16;
    if !settings.trigger.stages.is_empty() {
        mode |= 1;
    }
    if settings.external_clock {
        mode |= 1 << 1;
    }
    if settings.external_clock_active_edge {
        mode |= 1 << 2;
    }
    if settings.run_length {
        mode |= 1 << 3;
    }
    if settings.sample_rate_hz == 500_000_000 {
        mode |= 1 << 5;
    }
    if settings.sample_rate_hz == 1_000_000_000 {
        mode |= 1 << 6;
    }
    if settings.input_filter {
        mode |= 1 << 8;
    }
    if div_ceil(settings.sample_rate_hz * u64::from(plan.channels), 8_000)? < 1024 {
        mode |= 1 << 10;
    }
    if settings.trigger.serial {
        mode |= 1 << 11;
    }
    if settings.mode == CaptureMode::Streaming {
        mode |= 1 << 12;
    }
    let mut p = [0u8; 672];
    put32(&mut p, 0, 0xf5a5_f5a5);
    put16(&mut p, 4, 1);
    put16(&mut p, 6, mode);
    put16(&mut p, 8, 0x0102);
    put16(&mut p, 10, d as u16);
    put16(&mut p, 12, divider_high);
    put16(&mut p, 14, 0x0302);
    put32(&mut p, 16, (plan.actual_samples >> 4) as u32);
    put16(&mut p, 20, 0x0502);
    put32(&mut p, 22, trigger_position as u32);
    put16(&mut p, 26, 0x0701);
    let stages = settings.trigger.stages.len() as u16;
    put16(
        &mut p,
        28,
        (u16::from(plan.channels) << 8) | if stages <= 1 { 0 } else { stages },
    );
    put16(&mut p, 30, 0x0802);
    put32(&mut p, 32, plan.actual_samples as u32);
    put16(&mut p, 36, 0x0a02);
    put16(&mut p, 38, settings.input_mask);
    put16(&mut p, 42, 0x0c01);
    put16(&mut p, 46, 0x40a0);
    for stage in 0..16 {
        let configured = settings.trigger.stages.get(stage);
        let (m0, v0, e0, l0, c0) = trigger_words(configured.map(|s| &s.plane0), configured);
        let (m1, v1, e1, l1, _) = trigger_words(configured.map(|s| &s.plane1), configured);
        put16(&mut p, 48 + stage * 2, replicate_1g(m0, settings, stage));
        put16(&mut p, 80 + stage * 2, replicate_1g(m1, settings, stage));
        put16(&mut p, 112 + stage * 2, replicate_1g(v0, settings, stage));
        put16(&mut p, 144 + stage * 2, replicate_1g(v1, settings, stage));
        put16(&mut p, 176 + stage * 2, replicate_1g(e0, settings, stage));
        put16(&mut p, 208 + stage * 2, replicate_1g(e1, settings, stage));
        put16(&mut p, 240 + stage * 2, l0);
        put16(&mut p, 272 + stage * 2, l1);
        put32(&mut p, 304 + stage * 4, c0);
    }
    put32(&mut p, 368, 0xfa5a_fa5a);
    Ok(p)
}
fn trigger_words(
    plane: Option<&[TriggerCondition; 16]>,
    stage: Option<&LogicTriggerStage>,
) -> (u16, u16, u16, u16, u32) {
    let Some(plane) = plane else {
        return (0xffff, 0, 0, 2, 0);
    };
    let mut mask = 0;
    let mut value = 0;
    let mut edge = 0;
    for (i, condition) in plane.iter().enumerate() {
        let bit = 1 << i;
        match condition {
            TriggerCondition::Ignore => mask |= bit,
            TriggerCondition::Low => {}
            TriggerCondition::High => value |= bit,
            TriggerCondition::Rising => {
                value |= bit;
                edge |= bit;
            }
            TriggerCondition::Falling => edge |= bit,
            TriggerCondition::Either => {
                mask |= bit;
                edge |= bit;
            }
        }
    }
    let stage = stage.unwrap();
    (
        mask,
        value,
        edge,
        trigger_logic_word(stage),
        stage.count,
    )
}
fn trigger_logic_word(stage: &LogicTriggerStage) -> u16 {
    let logic = match stage.logic {
        TriggerLogic::Or => 0,
        TriggerLogic::And => 1,
    };
    let contiguous = u16::from(stage.count_mode == TriggerCountMode::Consecutive);
    ((logic + 2 * contiguous) << 1) | u16::from(stage.inverted)
}
fn replicate_1g(word: u16, settings: &DsLogicCaptureSettings, stage: usize) -> u16 {
    if settings.sample_rate_hz == 1_000_000_000 && !(settings.trigger.serial && stage == 3) {
        (word & 0xff) | ((word & 0xff) << 8)
    } else {
        word
    }
}
fn put16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use serde::Deserialize;

    use signal_processing::{
        CaptureAcquisitionPhase, CaptureCursorItem, CaptureEvent, CaptureFailureKind,
        CaptureQueueReceiveError, CaptureSessionId, CaptureStoreCursor,
        CaptureStoreDescriptor, NativeCaptureStore, NativeCaptureStoreConfig,
        bounded_capture_event_queue,
    };

    use crate::live_capture::{
        AcquisitionContext, DsLogicU3Pro16BufferedProvider,
        DsLogicU3Pro16StreamingProvider,
    };

    use super::*;

    #[derive(Deserialize)]
    struct PacketFixture {
        sample_rate_hz: u64,
        input_mask: u16,
        sample_limit: u64,
        trigger_sample: u32,
        ram_start: u32,
        remaining_samples: u64,
        expected: PacketExpected,
    }

    #[derive(Deserialize)]
    struct PacketExpected {
        mode: u16,
        divider: u16,
        divider_high: u16,
        capture_units: u32,
        trigger_position: u32,
        channel_stage_word: u16,
        actual_samples: u32,
        trigger_mask0: u16,
        trigger_value0: u16,
        trigger_edge0: u16,
        trigger_logic0: u16,
    }

    #[derive(Deserialize)]
    struct AdvancedTriggerPacketFixture {
        sample_rate_hz: u64,
        input_mask: u16,
        sample_limit: u64,
        expected: AdvancedTriggerPacketExpected,
    }

    #[derive(Deserialize)]
    struct AdvancedTriggerPacketExpected {
        channel_stage_word: u16,
        mask0: [u16; 2],
        value0: [u16; 2],
        edge0: [u16; 2],
        logic0: [u16; 2],
        count0: [u32; 2],
    }

    fn packet_fixture() -> PacketFixture {
        serde_json::from_str(include_str!(
            "../../test_data/dslogic_u3pro16/buffered_packet.json"
        ))
        .unwrap()
    }

    fn advanced_trigger_packet_fixture() -> AdvancedTriggerPacketFixture {
        serde_json::from_str(include_str!(
            "../../test_data/dslogic_u3pro16/advanced_trigger_packet.json"
        ))
        .unwrap()
    }

    fn fixture_config(fixture: &PacketFixture) -> LogicCaptureConfig {
        let mut stage = LogicTriggerStage::default();
        stage.plane0[0] = TriggerCondition::Rising;
        stage.plane0[2] = TriggerCondition::High;
        LogicCaptureConfig {
            trigger: LogicTrigger {
                stages: vec![stage],
                serial: false,
            },
            trigger_percent: 50,
            encoding: LogicEncodingRequest::RunLength,
            ..LogicCaptureConfig::finite(
                fixture.sample_rate_hz,
                u64::from(fixture.input_mask),
                fixture.sample_limit,
            )
        }
    }

    fn fixture_header(fixture: &PacketFixture) -> Vec<u8> {
        let mut header = vec![0_u8; 1024];
        put32(&mut header, 0, 0x5555_5555);
        put32(&mut header, 4, fixture.trigger_sample);
        put32(&mut header, 8, fixture.ram_start);
        put32(&mut header, 12, fixture.remaining_samples as u32);
        put32(&mut header, 16, (fixture.remaining_samples >> 32) as u32);
        put32(&mut header, 20, 1);
        header
    }

    fn get16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn get32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    struct FixtureTransport {
        control_reads: VecDeque<Vec<u8>>,
        bulk_reads: VecDeque<Vec<u8>>,
        bulk_writes: Arc<Mutex<Vec<Vec<u8>>>>,
        idle_stream: bool,
    }

    impl FixtureTransport {
        fn control_reads() -> VecDeque<Vec<u8>> {
            VecDeque::from([
                vec![0x40],
                vec![0x0e],
                vec![0x0e],
                vec![2, 0],
                vec![0x08],
                vec![0x80],
            ])
        }

        fn new(header: Vec<u8>, data: Vec<u8>) -> (Self, Arc<Mutex<Vec<Vec<u8>>>>) {
            const UNALIGNED_TRANSFER_BYTES: usize = 257;

            assert!(data.len() > UNALIGNED_TRANSFER_BYTES);
            let bulk_writes = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    control_reads: Self::control_reads(),
                    bulk_reads: VecDeque::from([
                        header,
                        data[..UNALIGNED_TRANSFER_BYTES].to_vec(),
                        data[UNALIGNED_TRANSFER_BYTES..].to_vec(),
                    ]),
                    bulk_writes: Arc::clone(&bulk_writes),
                    idle_stream: false,
                },
                bulk_writes,
            )
        }

        fn overflowing_stream(
            header: Vec<u8>,
        ) -> (Self, Arc<Mutex<Vec<Vec<u8>>>>) {
            let bulk_writes = Arc::new(Mutex::new(Vec::new()));
            let mut control_reads = Self::control_reads();
            control_reads.push_back(vec![0x10]);
            (
                Self {
                    control_reads,
                    bulk_reads: VecDeque::from([header]),
                    bulk_writes: Arc::clone(&bulk_writes),
                    idle_stream: false,
                },
                bulk_writes,
            )
        }

        fn idle_stream(header: Vec<u8>) -> Self {
            Self {
                control_reads: Self::control_reads(),
                bulk_reads: VecDeque::from([header]),
                bulk_writes: Arc::new(Mutex::new(Vec::new())),
                idle_stream: true,
            }
        }
    }

    impl UsbTransport for FixtureTransport {
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::Super
        }

        fn control_write(
            &mut self,
            _request_type: u8,
            _request: u8,
            _value: u16,
            _index: u16,
            data: &[u8],
            _timeout: Duration,
        ) -> Result<usize, UsbError> {
            Ok(data.len())
        }

        fn control_read(
            &mut self,
            _request_type: u8,
            _request: u8,
            _value: u16,
            _index: u16,
            data: &mut [u8],
            _timeout: Duration,
        ) -> Result<usize, UsbError> {
            let response = match self.control_reads.pop_front() {
                Some(response) => response,
                None if self.idle_stream => vec![0; data.len()],
                None => return Err(UsbError::Other),
            };
            if response.len() != data.len() {
                return Err(UsbError::Other);
            }
            data.copy_from_slice(&response);
            Ok(data.len())
        }

        fn bulk_write(
            &mut self,
            _endpoint: u8,
            data: &[u8],
            _timeout: Duration,
        ) -> Result<usize, UsbError> {
            self.bulk_writes.lock().unwrap().push(data.to_vec());
            Ok(data.len())
        }

        fn bulk_read(
            &mut self,
            _endpoint: u8,
            data: &mut [u8],
            _timeout: Duration,
        ) -> Result<usize, UsbError> {
            let response = match self.bulk_reads.pop_front() {
                Some(response) => response,
                None if self.idle_stream => {
                    std::thread::sleep(Duration::from_millis(20));
                    return Err(UsbError::Timeout);
                }
                None => return Err(UsbError::Timeout),
            };
            if response.len() > data.len() {
                return Err(UsbError::Other);
            }
            data[..response.len()].copy_from_slice(&response);
            Ok(response.len())
        }
    }

    struct GeneratedStreamingTransport {
        control_reads: VecDeque<Vec<u8>>,
        header_pending: bool,
        data_bytes: usize,
        data_offset: usize,
    }

    impl GeneratedStreamingTransport {
        fn new(data_bytes: usize) -> Self {
            Self {
                control_reads: FixtureTransport::control_reads(),
                header_pending: true,
                data_bytes,
                data_offset: 0,
            }
        }
    }

    impl UsbTransport for GeneratedStreamingTransport {
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::Super
        }

        fn control_write(
            &mut self,
            _request_type: u8,
            _request: u8,
            _value: u16,
            _index: u16,
            data: &[u8],
            _timeout: Duration,
        ) -> Result<usize, UsbError> {
            Ok(data.len())
        }

        fn control_read(
            &mut self,
            _request_type: u8,
            _request: u8,
            _value: u16,
            _index: u16,
            data: &mut [u8],
            _timeout: Duration,
        ) -> Result<usize, UsbError> {
            let response = self.control_reads.pop_front().ok_or(UsbError::Other)?;
            if response.len() != data.len() {
                return Err(UsbError::Other);
            }
            data.copy_from_slice(&response);
            Ok(data.len())
        }

        fn bulk_write(
            &mut self,
            _endpoint: u8,
            data: &[u8],
            _timeout: Duration,
        ) -> Result<usize, UsbError> {
            Ok(data.len())
        }

        fn bulk_read(
            &mut self,
            _endpoint: u8,
            data: &mut [u8],
            _timeout: Duration,
        ) -> Result<usize, UsbError> {
            if self.header_pending {
                if data.len() != 1024 {
                    return Err(UsbError::Other);
                }
                data.fill(0);
                put32(data, 0, 0x5555_5555);
                self.header_pending = false;
                return Ok(data.len());
            }
            let read = data.len().min(self.data_bytes - self.data_offset);
            data[..read].fill(0xa5);
            self.data_offset += read;
            Ok(read)
        }
    }

    #[test]
    fn buffered_packet_and_trigger_header_match_checked_in_fixture() {
        let fixture = packet_fixture();
        let config = fixture_config(&fixture);
        let mut settings = DsLogicCaptureSettings::finite(
            config.sample_rate_hz,
            config.input_mask as u16,
            config.sample_limit,
        );
        settings.trigger = config.trigger;
        settings.run_length = config.encoding == LogicEncodingRequest::RunLength;
        let plan = build_plan(LinkSpeed::Super, &settings).unwrap();
        let packet = build_settings_packet(LinkSpeed::Super, &settings, plan).unwrap();
        let expected = &fixture.expected;

        assert_eq!(get16(&packet, 6), expected.mode);
        assert_eq!(get16(&packet, 10), expected.divider);
        assert_eq!(get16(&packet, 12), expected.divider_high);
        assert_eq!(get32(&packet, 16), expected.capture_units);
        assert_eq!(get32(&packet, 22), expected.trigger_position);
        assert_eq!(get16(&packet, 28), expected.channel_stage_word);
        assert_eq!(get32(&packet, 32), expected.actual_samples);
        assert_eq!(get16(&packet, 48), expected.trigger_mask0);
        assert_eq!(get16(&packet, 112), expected.trigger_value0);
        assert_eq!(get16(&packet, 176), expected.trigger_edge0);
        assert_eq!(get16(&packet, 240), expected.trigger_logic0);

        let header = translate_trigger_header(&fixture_header(&fixture), CaptureMode::Finite, plan)
            .unwrap();
        assert_eq!(header.trigger_sample(), Some(u64::from(fixture.trigger_sample)));
        assert_eq!(header.captured_samples(), fixture.sample_limit);
        assert_eq!(header.remaining_samples(), fixture.remaining_samples);
        assert_eq!(header.ram_start(), fixture.ram_start);
    }

    #[test]
    fn advanced_trigger_stages_match_checked_in_packet_fixture() {
        let fixture = advanced_trigger_packet_fixture();
        let mut first = LogicTriggerStage::default();
        first.plane0[0] = TriggerCondition::Rising;
        first.plane0[2] = TriggerCondition::High;
        first.logic = TriggerLogic::And;
        first.inverted = true;
        first.count = 3;
        let mut second = LogicTriggerStage::default();
        second.plane0[2] = TriggerCondition::Low;
        second.plane0[5] = TriggerCondition::Falling;
        second.logic = TriggerLogic::Or;
        second.count_mode = TriggerCountMode::Consecutive;
        second.count = 5;
        let mut settings = DsLogicCaptureSettings::finite(
            fixture.sample_rate_hz,
            fixture.input_mask,
            fixture.sample_limit,
        );
        settings.trigger = LogicTrigger {
            stages: vec![first, second],
            serial: false,
        };

        let plan = build_plan(LinkSpeed::Super, &settings).unwrap();
        let packet = build_settings_packet(LinkSpeed::Super, &settings, plan).unwrap();
        let expected = fixture.expected;

        assert_eq!(get16(&packet, 28), expected.channel_stage_word);
        for stage in 0..2 {
            assert_eq!(get16(&packet, 48 + stage * 2), expected.mask0[stage]);
            assert_eq!(get16(&packet, 112 + stage * 2), expected.value0[stage]);
            assert_eq!(get16(&packet, 176 + stage * 2), expected.edge0[stage]);
            assert_eq!(get16(&packet, 240 + stage * 2), expected.logic0[stage]);
            assert_eq!(get32(&packet, 304 + stage * 4), expected.count0[stage]);
        }
    }

    #[test]
    fn buffered_provider_uploads_fixture_losslessly_and_publishes_actual_trigger() {
        let fixture = packet_fixture();
        let config = fixture_config(&fixture);
        let data = (0..768).map(|index| (index * 37) as u8).collect::<Vec<_>>();
        let expected_data = data.clone();
        let (transport, settings_writes) =
            FixtureTransport::new(fixture_header(&fixture), data);
        let analyzer = DsLogicU3Pro16::new(transport).unwrap();
        let channels = vec![
            signal_processing::CaptureChannelId::new("u3pro16:input:0"),
            signal_processing::CaptureChannelId::new("u3pro16:input:2"),
            signal_processing::CaptureChannelId::new("u3pro16:input:5"),
        ];
        let provider =
            DsLogicU3Pro16BufferedProvider::new(analyzer, config, channels.clone()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let session_id = CaptureSessionId::new(0x8316);
        let descriptor = CaptureStoreDescriptor::new(session_id, channels).unwrap();
        let (store, writer) = NativeCaptureStore::create(
            NativeCaptureStoreConfig::new(directory.path(), descriptor)
                .with_commit_batch_chunks(1)
                .unwrap(),
        )
        .unwrap();
        let _paused_cursor = store.open_cursor().unwrap();
        let (events, event_reader) = bounded_capture_event_queue(64).unwrap();
        let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
        let mut acquisition = provider.prepare(context).unwrap();

        acquisition.start().unwrap();
        let outcome = acquisition.join().unwrap();
        assert_eq!(outcome.captured_samples, fixture.sample_limit);
        assert_eq!(outcome.chunk_count, 2);
        assert!(!outcome.stopped);
        assert_eq!(store.snapshot().resident_commit_records, 0);
        assert_eq!(settings_writes.lock().unwrap()[0].len(), 672);

        let finalized = store.finalize().unwrap();
        let mut cursor = finalized.open_cursor().unwrap();
        let mut sample = 0_u64;
        loop {
            match cursor.next().unwrap() {
                CaptureCursorItem::Chunk(chunk) => {
                    assert_eq!(chunk.start_sample(), sample);
                    for relative_sample in 0..chunk.sample_count() {
                        for channel in 0..3 {
                            let source_bit = ((sample + relative_sample) * 3) as usize + channel;
                            let expected = expected_data[source_bit / 8] & (1 << (source_bit % 8)) != 0;
                            assert_eq!(
                                chunk.packed_level(relative_sample, channel),
                                Some(expected),
                                "sample {} channel {channel}",
                                sample + relative_sample
                            );
                        }
                    }
                    sample += chunk.sample_count();
                }
                CaptureCursorItem::End => break,
                CaptureCursorItem::Pending => panic!("finalized fixture cursor cannot be pending"),
            }
        }
        assert_eq!(sample, fixture.sample_limit);

        let mut trigger_sample = None;
        let mut phases = Vec::new();
        loop {
            match event_reader.try_recv() {
                Ok(CaptureEvent::Triggered { sample, .. }) => trigger_sample = Some(sample),
                Ok(CaptureEvent::Status(status)) => phases.push(status.phase),
                Ok(CaptureEvent::Progress { .. } | CaptureEvent::Health { .. }) => {}
                Ok(CaptureEvent::Plan { .. }) => {}
                Ok(CaptureEvent::Failed(failure)) => panic!("fixture failed: {failure:?}"),
                Err(CaptureQueueReceiveError::Closed) => break,
                Err(CaptureQueueReceiveError::Empty) => std::thread::yield_now(),
                Err(CaptureQueueReceiveError::Timeout) => unreachable!(),
            }
        }
        assert_eq!(trigger_sample, Some(u64::from(fixture.trigger_sample)));
        assert!(phases.contains(&CaptureAcquisitionPhase::CapturingOnDevice));
        assert!(phases.contains(&CaptureAcquisitionPhase::UploadingBufferedData));
    }

    #[test]
    fn streaming_provider_commits_live_fixture_and_stops_at_the_host_limit() {
        let fixture = packet_fixture();
        let mut config = fixture_config(&fixture);
        config.mode = CaptureMode::Streaming;
        let data = (0..768).map(|index| (index * 37) as u8).collect::<Vec<_>>();
        let expected_data = data.clone();
        let (transport, _) = FixtureTransport::new(fixture_header(&fixture), data);
        let analyzer = DsLogicU3Pro16::new(transport).unwrap();
        let channels = vec![
            signal_processing::CaptureChannelId::new("u3pro16:input:0"),
            signal_processing::CaptureChannelId::new("u3pro16:input:2"),
            signal_processing::CaptureChannelId::new("u3pro16:input:5"),
        ];
        let provider =
            DsLogicU3Pro16StreamingProvider::new(analyzer, config, channels.clone()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let session_id = CaptureSessionId::new(0x8317);
        let descriptor = CaptureStoreDescriptor::new(session_id, channels).unwrap();
        let (store, writer) = NativeCaptureStore::create(
            NativeCaptureStoreConfig::new(directory.path(), descriptor)
                .with_commit_batch_chunks(1)
                .unwrap(),
        )
        .unwrap();
        let _paused_cursor = store.open_cursor().unwrap();
        let (events, event_reader) = bounded_capture_event_queue(64).unwrap();
        let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
        let mut acquisition = provider.prepare(context).unwrap();

        acquisition.start().unwrap();
        let outcome = acquisition.join().unwrap();
        assert_eq!(outcome.captured_samples, fixture.sample_limit);
        assert_eq!(outcome.chunk_count, 2);
        assert!(!outcome.stopped);
        assert_eq!(store.snapshot().resident_commit_records, 0);

        let finalized = store.finalize().unwrap();
        let mut cursor = finalized.open_cursor().unwrap();
        let mut sample = 0_u64;
        loop {
            match cursor.next().unwrap() {
                CaptureCursorItem::Chunk(chunk) => {
                    assert_eq!(chunk.start_sample(), sample);
                    for relative_sample in 0..chunk.sample_count() {
                        for channel in 0..3 {
                            let source_bit = ((sample + relative_sample) * 3) as usize + channel;
                            let expected =
                                expected_data[source_bit / 8] & (1 << (source_bit % 8)) != 0;
                            assert_eq!(
                                chunk.packed_level(relative_sample, channel),
                                Some(expected)
                            );
                        }
                    }
                    sample += chunk.sample_count();
                }
                CaptureCursorItem::End => break,
                CaptureCursorItem::Pending => panic!("finalized stream cannot be pending"),
            }
        }
        assert_eq!(sample, fixture.sample_limit);

        let mut phases = Vec::new();
        loop {
            match event_reader.try_recv() {
                Ok(CaptureEvent::Status(status)) => phases.push(status.phase),
                Ok(
                    CaptureEvent::Triggered { .. }
                    | CaptureEvent::Progress { .. }
                    | CaptureEvent::Health { .. }
                    | CaptureEvent::Plan { .. },
                ) => {}
                Ok(CaptureEvent::Failed(failure)) => panic!("stream failed: {failure:?}"),
                Err(CaptureQueueReceiveError::Closed) => break,
                Err(CaptureQueueReceiveError::Empty) => std::thread::yield_now(),
                Err(CaptureQueueReceiveError::Timeout) => unreachable!(),
            }
        }
        assert!(phases.contains(&CaptureAcquisitionPhase::ReceivingLiveData));
    }

    #[test]
    fn streaming_overflow_is_an_explicit_integrity_error() {
        let fixture = packet_fixture();
        let mut config = fixture_config(&fixture);
        config.mode = CaptureMode::Streaming;
        let (transport, _) = FixtureTransport::overflowing_stream(fixture_header(&fixture));
        let mut analyzer = DsLogicU3Pro16::new(transport).unwrap();
        analyzer.prepare_streaming_capture(&config).unwrap();
        analyzer.start_capture().unwrap();

        let error = analyzer.next_chunk().unwrap_err();

        assert!(matches!(error, LogicAnalyzerError::Integrity(_)));
        assert!(error.to_string().contains("overflow"));
    }

    #[test]
    fn streaming_provider_publishes_overflow_as_an_integrity_failure() {
        let fixture = packet_fixture();
        let mut config = fixture_config(&fixture);
        config.mode = CaptureMode::Streaming;
        let (transport, _) = FixtureTransport::overflowing_stream(fixture_header(&fixture));
        let analyzer = DsLogicU3Pro16::new(transport).unwrap();
        let channels = vec![
            signal_processing::CaptureChannelId::new("u3pro16:input:0"),
            signal_processing::CaptureChannelId::new("u3pro16:input:2"),
            signal_processing::CaptureChannelId::new("u3pro16:input:5"),
        ];
        let provider =
            DsLogicU3Pro16StreamingProvider::new(analyzer, config, channels.clone()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let session_id = CaptureSessionId::new(0x8319);
        let descriptor = CaptureStoreDescriptor::new(session_id, channels).unwrap();
        let (_store, writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
            directory.path(),
            descriptor,
        ))
        .unwrap();
        let (events, event_reader) = bounded_capture_event_queue(64).unwrap();
        let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
        let mut acquisition = provider.prepare(context).unwrap();
        acquisition.start().unwrap();

        let error = acquisition.join().unwrap_err();

        assert!(matches!(error, crate::live_capture::AcquisitionError::Integrity(_)));
        let mut failure = None;
        loop {
            match event_reader.try_recv() {
                Ok(CaptureEvent::Failed(event)) => failure = Some(event),
                Ok(_) => {}
                Err(CaptureQueueReceiveError::Closed) => break,
                Err(CaptureQueueReceiveError::Empty) => std::thread::yield_now(),
                Err(CaptureQueueReceiveError::Timeout) => unreachable!(),
            }
        }
        assert_eq!(failure.unwrap().kind, CaptureFailureKind::Integrity);
    }

    #[test]
    fn streaming_stop_interrupts_an_idle_capture_after_a_bounded_read() {
        let fixture = packet_fixture();
        let mut config = fixture_config(&fixture);
        config.mode = CaptureMode::Streaming;
        config.sample_limit = 1_000_000;
        let transport = FixtureTransport::idle_stream(fixture_header(&fixture));
        let analyzer = DsLogicU3Pro16::new(transport).unwrap();
        let channels = vec![
            signal_processing::CaptureChannelId::new("u3pro16:input:0"),
            signal_processing::CaptureChannelId::new("u3pro16:input:2"),
            signal_processing::CaptureChannelId::new("u3pro16:input:5"),
        ];
        let provider =
            DsLogicU3Pro16StreamingProvider::new(analyzer, config, channels.clone()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let session_id = CaptureSessionId::new(0x8318);
        let descriptor = CaptureStoreDescriptor::new(session_id, channels).unwrap();
        let (store, writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
            directory.path(),
            descriptor,
        ))
        .unwrap();
        let (events, _event_reader) = bounded_capture_event_queue(64).unwrap();
        let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
        let mut acquisition = provider.prepare(context).unwrap();
        acquisition.start().unwrap();
        std::thread::sleep(Duration::from_millis(5));

        acquisition.request_stop().unwrap();
        let outcome = acquisition.join().unwrap();

        assert!(outcome.stopped);
        assert_eq!(outcome.captured_samples, 0);
        assert_eq!(store.finalize().unwrap().manifest().committed_samples, 0);
    }

    #[test]
    fn streaming_plan_enforces_link_speed_and_highest_enabled_input() {
        let mut config = LogicCaptureConfig::finite(100_000_000, 0b111, 4096);
        config.mode = CaptureMode::Streaming;
        assert!(u3pro16_streaming_plan(&config, LinkSpeed::High).is_ok());

        config.input_mask = 0b1001;
        assert!(u3pro16_streaming_plan(&config, LinkSpeed::High).is_err());
        assert!(u3pro16_streaming_plan(&config, LinkSpeed::Super).is_ok());

        config.sample_rate_hz = 250_000_000;
        config.input_mask = 1 << 12;
        assert!(u3pro16_streaming_plan(&config, LinkSpeed::Super).is_err());

        config.sample_rate_hz = 100_000;
        config.input_mask = 1;
        assert!(u3pro16_streaming_plan(&config, LinkSpeed::High).is_ok());
        assert!(u3pro16_streaming_plan(&config, LinkSpeed::Super).is_err());
    }

    #[test]
    #[ignore = "release-mode sustained-ingest benchmark; run with --release --ignored benchmark_streaming_ingest"]
    fn benchmark_streaming_ingest_store_summary_and_consumer_lag() {
        use signal_processing::live_capture_waveform::NativeGrowingCaptureIndex;

        for (channels_count, rate_hz, samples) in [
            (3_usize, 1_000_000_000_u64, 32_000_000_u64),
            (16, 125_000_000, 32_000_000),
        ] {
            let input_mask = (1_u64 << channels_count) - 1;
            let mut config = LogicCaptureConfig::finite(rate_hz, input_mask, samples);
            config.mode = CaptureMode::Streaming;
            let data_bytes = usize::try_from(
                u128::from(samples)
                    .checked_mul(channels_count as u128)
                    .unwrap()
                    .div_ceil(8),
            )
            .unwrap();
            let analyzer =
                DsLogicU3Pro16::new(GeneratedStreamingTransport::new(data_bytes)).unwrap();
            let channels = (0..channels_count)
                .map(|channel| {
                    signal_processing::CaptureChannelId::new(format!(
                        "u3pro16:input:{channel}"
                    ))
                })
                .collect::<Vec<_>>();
            let provider =
                DsLogicU3Pro16StreamingProvider::new(analyzer, config, channels.clone()).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let session_id = CaptureSessionId::new(0x9000 + channels_count as u128);
            let descriptor = CaptureStoreDescriptor::new(session_id, channels.clone()).unwrap();
            let (store, writer) = NativeCaptureStore::create(
                NativeCaptureStoreConfig::new(directory.path(), descriptor),
            )
            .unwrap();
            let (index, index_worker) = NativeGrowingCaptureIndex::spawn(
                store.clone(),
                "U3 streaming benchmark",
                rate_hz as f64,
                (0..channels_count)
                    .map(|channel| format!("Ch {channel}"))
                    .collect(),
            )
            .unwrap();
            let analyzed_samples = Arc::new(AtomicU64::new(0));
            let analyzed_samples_worker = Arc::clone(&analyzed_samples);
            let mut slow_cursor = store.open_cursor().unwrap();
            let slow_consumer = std::thread::spawn(move || loop {
                match slow_cursor.wait_next(Duration::from_millis(50)).unwrap() {
                    CaptureCursorItem::Chunk(chunk) => {
                        analyzed_samples_worker.store(chunk.end_sample(), Ordering::Relaxed);
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    CaptureCursorItem::Pending => {}
                    CaptureCursorItem::End => break,
                }
            });
            let (events, _event_reader) = bounded_capture_event_queue(4096).unwrap();
            let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
            let mut acquisition = provider.prepare(context).unwrap();

            let started = Instant::now();
            acquisition.start().unwrap();
            let outcome = acquisition.join().unwrap();
            let acquisition_elapsed = started.elapsed();
            let lag_at_finish = samples.saturating_sub(analyzed_samples.load(Ordering::Relaxed));
            let summary_started = Instant::now();
            index_worker.join().unwrap();
            slow_consumer.join().unwrap();
            let catch_up_elapsed = summary_started.elapsed();
            store.finalize().unwrap();

            let mib = data_bytes as f64 / (1024.0 * 1024.0);
            eprintln!(
                "u3-stream channels={channels_count} rate_hz={rate_hz} samples={samples} data_mib={mib:.1} acquisition_s={:.3} ingest_mib_s={:.1} optional_consumer_lag_samples={lag_at_finish} summary_catchup_s={:.3} resident_summary_records={}",
                acquisition_elapsed.as_secs_f64(),
                mib / acquisition_elapsed.as_secs_f64(),
                catch_up_elapsed.as_secs_f64(),
                index.resident_summary_records(),
            );
            assert_eq!(outcome.captured_samples, samples);
            assert_eq!(store.snapshot().resident_commit_records, 0);
            assert!(index.resident_summary_records() <= channels_count * 64 * 12);
        }
    }

    #[test]
    fn finite_packet_has_protocol_markers() {
        let settings = DsLogicCaptureSettings::finite(100_000_000, 0xffff, 4096);
        let plan = build_plan(LinkSpeed::Super, &settings).unwrap();
        let packet = build_settings_packet(LinkSpeed::Super, &settings, plan).unwrap();
        assert_eq!(&packet[0..4], &0xf5a5_f5a5u32.to_le_bytes());
        assert_eq!(&packet[368..372], &0xfa5a_fa5au32.to_le_bytes());
        assert_eq!(packet.len(), 672);
    }
    #[test]
    fn rejects_high_speed_wide_stream_rate() {
        let settings = DsLogicCaptureSettings {
            mode: CaptureMode::Streaming,
            ..DsLogicCaptureSettings::finite(100_000_000, 0xffff, 4096)
        };
        assert!(build_plan(LinkSpeed::High, &settings).is_err());
    }
    #[test]
    fn retains_narrow_mode_rate() {
        let settings = DsLogicCaptureSettings {
            mode: CaptureMode::Streaming,
            ..DsLogicCaptureSettings::finite(100_000_000, 0x0007, 4096)
        };
        assert!(build_plan(LinkSpeed::High, &settings).is_ok());
    }
}
