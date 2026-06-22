//! DSLogic U3Pro16 USB driver.
//!
//! The wire protocol is kept here, below the generic `LogicAnalyzer` boundary.
//! `RusbTransport` is deliberately small so a libsigrok-backed transport can be
//! added without changing capture packet construction or graph integration.

use super::logic_analyzer::{
    CaptureMode, ClockEdge, ClockSource, LogicAnalyzer, LogicAnalyzerError, LogicAnalyzerInfo,
    LogicAnalyzerResult, LogicAnalyzerSource, LogicCaptureConfig, LogicChunk, LogicEncoding,
    LogicEncodingRequest, LogicTrigger, LogicTriggerStage, TriggerCondition,
};
use rusb::{Context, DeviceHandle, UsbContext};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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
    plan: Option<CapturePlan>,
    active: bool,
    header_pending: bool,
    bytes_remaining: Option<usize>,
    bit_position: u64,
}

#[derive(Clone, Copy)]
struct CapturePlan {
    channels: u8,
    actual_samples: u64,
    actual_bytes: usize,
    stream_buffer: usize,
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
        self.poll_status(0x40)?;
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
        let until = Instant::now() + STATUS_TIMEOUT;
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
    fn plan(&self) -> LogicAnalyzerResult<CapturePlan> {
        build_plan(self.transport.link_speed(), &self.settings)
    }
    fn settings_packet(&self, plan: CapturePlan) -> LogicAnalyzerResult<[u8; 672]> {
        build_settings_packet(self.transport.link_speed(), &self.settings, plan)
    }
}

impl<T: UsbTransport> LogicAnalyzer for DsLogicU3Pro16<T> {
    fn info(&self) -> &LogicAnalyzerInfo {
        &self.info
    }
    fn configure_capture(&mut self, config: &LogicCaptureConfig) -> LogicAnalyzerResult<()> {
        if config.input_mask > u64::from(u16::MAX) {
            return Err(LogicAnalyzerError::InvalidSettings(
                "U3Pro16 has only 16 inputs".into(),
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
        self.settings = settings;
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
        let status = self.command_read_byte(2, 0)?;
        if status & 0x40 == 0 {
            return Err(LogicAnalyzerError::Protocol(
                "FPGA is not configured; call configure_fpga with the U3Pro16 image".into(),
            ));
        }
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
        let plan = self.plan()?;
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
        self.command_write(8, 0, &[])?;
        self.plan = Some(plan);
        self.active = true;
        self.header_pending = true;
        self.bytes_remaining = None;
        self.bit_position = 0;
        Ok(())
    }
    fn next_chunk(&mut self) -> LogicAnalyzerResult<Option<LogicChunk>> {
        if !self.active {
            return Err(LogicAnalyzerError::NotCapturing);
        }
        let plan = self.plan.ok_or(LogicAnalyzerError::NotCapturing)?;
        if self.header_pending {
            let mut header = [0u8; 1024];
            let read = usb(
                self.transport.bulk_read(BULK_IN, &mut header, BULK_TIMEOUT),
                "trigger header read",
            )?;
            if read != header.len() {
                return Err(LogicAnalyzerError::Protocol(format!(
                    "short trigger header: {read}/1024 bytes"
                )));
            }
            if u32::from_le_bytes(header[0..4].try_into().unwrap()) != 0x5555_5555 {
                return Err(LogicAnalyzerError::Protocol(
                    "invalid trigger-header magic".into(),
                ));
            }
            let remaining = u32::from_le_bytes(header[12..16].try_into().unwrap()) as u64
                | ((u32::from_le_bytes(header[16..20].try_into().unwrap()) as u64) << 32);
            if self.settings.mode == CaptureMode::Finite {
                if remaining >= plan.actual_samples {
                    return Err(LogicAnalyzerError::Protocol(
                        "trigger header remaining count is outside capture limit".into(),
                    ));
                }
                let delivered_samples = (plan.actual_samples - remaining) & !1023;
                let delivered_bytes = delivered_samples
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
            return if self.settings.mode == CaptureMode::Streaming {
                Ok(Some(self.empty_chunk(plan)))
            } else {
                Ok(None)
            };
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
            encoding: if self.settings.run_length {
                LogicEncoding::Opaque
            } else {
                LogicEncoding::InterleavedLsbFirst
            },
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
    fn empty_chunk(&self, plan: CapturePlan) -> LogicChunk {
        LogicChunk {
            data: Arc::from([]),
            bit_offset: 0,
            bit_len: 0,
            channel_count: plan.channels,
            start_bit: self.bit_position,
            encoding: if self.settings.run_length {
                LogicEncoding::Opaque
            } else {
                LogicEncoding::InterleavedLsbFirst
            },
        }
    }
}

impl<T: UsbTransport> Drop for DsLogicU3Pro16<T> {
    fn drop(&mut self) {
        let _ = self.stop_capture();
        let _ = self.transport.close();
    }
}

fn build_plan(
    speed: LinkSpeed,
    settings: &DsLogicCaptureSettings,
) -> LogicAnalyzerResult<CapturePlan> {
    if !RATES.contains(&settings.sample_rate_hz) {
        return Err(LogicAnalyzerError::InvalidSettings(
            "sample rate must be one of the device's discrete supported rates".into(),
        ));
    }
    let channels = settings.input_mask.count_ones() as u8;
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
        .checked_div(64)
        .and_then(|v| v.checked_mul(u64::from(channels)))
        .and_then(|v| v.checked_mul(8))
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
    Ok(CapturePlan {
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
    plan: CapturePlan,
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
        (stage.logic.wire() << 1) | u16::from(stage.inverted),
        stage.count,
    )
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
    use super::*;
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
