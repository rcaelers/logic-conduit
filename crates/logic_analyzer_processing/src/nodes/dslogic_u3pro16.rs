//! DSLogic U3Pro16 USB processing-node driver.
//!
//! The wire protocol is kept here, below the generic `LogicAnalyzer` boundary.
//! `RusbTransport` is deliberately small so a libsigrok-backed transport can be
//! added without changing capture packet construction or graph integration.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
const FPGA_DONE_TIMEOUT: Duration = Duration::from_secs(5);
const FPGA_UPLOAD_SETTLE: Duration = Duration::from_millis(20);
const STREAMING_DATA_RECEIVE_DEPTH: u8 = 4;
/// Target half a millisecond of signal time per host-streaming transfer.
/// Four queued transfers preserve USB throughput while keeping the written
/// prefix close enough to the device for interactive waveform following.
const STREAMING_TRANSFER_PARTS_PER_MS: u64 = 2;
const FPGA_DIVIDER_CLOCK_HZ: u64 = 500_000_000;
const FPGA_PRE_DIVIDER: u64 = 5;
const ADC_CONTROL_ADDRESS: u16 = 0x48;
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
pub(super) struct DsLogicCaptureSettings {
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
    pub(super) fn finite(sample_rate_hz: u64, input_mask: u16, sample_limit: u64) -> Self {
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

fn settings_from_config(
    config: &LogicCaptureConfig,
) -> LogicAnalyzerResult<DsLogicCaptureSettings> {
    if config.input_mask > u64::from(u16::MAX) {
        return Err(LogicAnalyzerError::InvalidSettings(
            "U3Pro16 has only 16 inputs".into(),
        ));
    }
    if config
        .threshold_volts
        .is_some_and(|volts| !volts.is_finite())
    {
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

/// Failure reported by a [`UsbTransport`] operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbError {
    /// The operation did not complete before its deadline.
    Timeout,
    /// The transport failed for a reason other than a timeout.
    Other,
}

/// USB operations required by the U3Pro16 protocol.
///
/// Implementations must preserve call order. The queued-read methods may use an
/// asynchronous backend; transports without that capability can retain the
/// default synchronous fallback.
pub trait UsbTransport: Send + 'static {
    /// Returns the negotiated USB link speed.
    fn link_speed(&self) -> LinkSpeed;
    /// Performs one USB control write.
    fn control_write(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, UsbError>;
    /// Performs one USB control read.
    fn control_read(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, UsbError>;
    /// Writes one bulk transfer.
    fn bulk_write(
        &mut self,
        endpoint: u8,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, UsbError>;
    /// Reads one bulk transfer.
    fn bulk_read(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, UsbError>;
    /// Queues one bulk receive before a device command that produces its
    /// response. Implementations without asynchronous USB support return
    /// `Ok(false)` and callers fall back to a synchronous receive.
    fn queue_bulk_read(
        &mut self,
        _endpoint: u8,
        _byte_len: usize,
        _timeout: Duration,
    ) -> Result<bool, UsbError> {
        Ok(false)
    }
    /// Takes the queued receive, waiting up to `timeout` for completion.
    /// `Ok(None)` means no receive was queued.
    fn take_queued_bulk_read(
        &mut self,
        _byte_len: usize,
        _timeout: Duration,
    ) -> Result<Option<Vec<u8>>, UsbError> {
        Ok(None)
    }
    /// Cancels an outstanding queued bulk read, if present.
    fn cancel_queued_bulk_read(&mut self) -> Result<(), UsbError> {
        Ok(())
    }
    /// Releases transport resources. Implementations must allow repeated calls.
    fn close(&mut self) -> Result<(), UsbError> {
        Ok(())
    }
}

/// Production `rusb` transport. It claims interface 0 during discovery.
pub struct RusbTransport {
    context: Context,
    handle: DeviceHandle<Context>,
    speed: LinkSpeed,
    claimed: bool,
    queued_bulk_reads: VecDeque<QueuedBulkRead>,
}

struct QueuedBulkRead {
    transfer: *mut rusb::ffi::libusb_transfer,
    buffer: Box<[u8]>,
    complete: Box<AtomicBool>,
}

// The transfer, its buffer, and completion flag are all owned by one
// `RusbTransport` and accessed serially by the capture worker.
unsafe impl Send for QueuedBulkRead {}

extern "system" fn mark_bulk_read_complete(transfer: *mut rusb::ffi::libusb_transfer) {
    // SAFETY: `user_data` points to `QueuedBulkRead::complete`, which remains
    // allocated until this completed transfer is freed.
    unsafe {
        let complete = (*transfer).user_data.cast::<AtomicBool>();
        (*complete).store(true, Ordering::Release);
    }
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
                context,
                handle,
                speed,
                claimed: true,
                queued_bulk_reads: VecDeque::new(),
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
                context,
                handle,
                speed,
                claimed: true,
                queued_bulk_reads: VecDeque::new(),
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
    fn queue_bulk_read(
        &mut self,
        endpoint: u8,
        byte_len: usize,
        _timeout: Duration,
    ) -> Result<bool, UsbError> {
        if self.queued_bulk_reads.len() == 8 {
            return Err(UsbError::Other);
        }
        let mut buffer = vec![0; byte_len].into_boxed_slice();
        let complete = Box::new(AtomicBool::new(false));
        // SAFETY: the transfer is initialized below and all referenced memory
        // stays owned by `QueuedBulkRead` until the transfer is completed and
        // freed in `take_queued_bulk_read` or `cancel_queued_bulk_read`.
        let transfer = unsafe { rusb::ffi::libusb_alloc_transfer(0) };
        if transfer.is_null() {
            return Err(UsbError::Other);
        }
        unsafe {
            rusb::ffi::libusb_fill_bulk_transfer(
                transfer,
                self.handle.as_raw(),
                endpoint,
                buffer.as_mut_ptr(),
                i32::try_from(byte_len).map_err(|_| UsbError::Other)?,
                mark_bulk_read_complete,
                (&raw const *complete).cast_mut().cast(),
                // The header may not arrive until a trigger occurs. Keep the
                // submitted USB request alive and let `take_queued_bulk_read`
                // perform bounded completion polls instead.
                0,
            );
            if rusb::ffi::libusb_submit_transfer(transfer) != 0 {
                rusb::ffi::libusb_free_transfer(transfer);
                return Err(UsbError::Other);
            }
        }
        self.queued_bulk_reads.push_back(QueuedBulkRead {
            transfer,
            buffer,
            complete,
        });
        tracing::debug!(endpoint, byte_len, "queued U3Pro16 bulk receive");
        Ok(true)
    }
    fn take_queued_bulk_read(
        &mut self,
        byte_len: usize,
        timeout: Duration,
    ) -> Result<Option<Vec<u8>>, UsbError> {
        if !self
            .queued_bulk_reads
            .iter()
            .any(|queued| queued.buffer.len() == byte_len)
        {
            tracing::debug!("no queued U3Pro16 bulk receive was available");
            return Ok(None);
        }
        let deadline = Instant::now() + timeout;
        let queued_index = loop {
            if let Some(index) = self.queued_bulk_reads.iter().position(|queued| {
                queued.buffer.len() == byte_len && queued.complete.load(Ordering::Acquire)
            }) {
                break index;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(UsbError::Timeout);
            }
            self.context
                .handle_events(Some(remaining))
                .map_err(map_usb_error)?;
        };
        let queued = self
            .queued_bulk_reads
            .remove(queued_index)
            .expect("queued read exists");
        // SAFETY: completion was observed, so libusb no longer accesses the
        // transfer or its buffer.
        let (status, actual_length) =
            unsafe { ((*queued.transfer).status, (*queued.transfer).actual_length) };
        unsafe { rusb::ffi::libusb_free_transfer(queued.transfer) };
        if status != rusb::constants::LIBUSB_TRANSFER_COMPLETED || actual_length < 0 {
            return Err(if status == rusb::constants::LIBUSB_TRANSFER_TIMED_OUT {
                UsbError::Timeout
            } else {
                UsbError::Other
            });
        }
        let actual_length = usize::try_from(actual_length).map_err(|_| UsbError::Other)?;
        if actual_length > queued.buffer.len() {
            return Err(UsbError::Other);
        }
        let mut buffer = queued.buffer.into_vec();
        buffer.truncate(actual_length);
        Ok(Some(buffer))
    }
    fn cancel_queued_bulk_read(&mut self) -> Result<(), UsbError> {
        while let Some(queued) = self.queued_bulk_reads.pop_front() {
            if !queued.complete.load(Ordering::Acquire) {
                // SAFETY: this is the only owner of the active transfer.
                if unsafe { rusb::ffi::libusb_cancel_transfer(queued.transfer) } != 0 {
                    // libusb may still access `queued`, so it must outlive this
                    // transport after a failed cancellation.
                    std::mem::forget(queued);
                    return Err(UsbError::Other);
                }
                let deadline = Instant::now() + BULK_TIMEOUT;
                while !queued.complete.load(Ordering::Acquire) {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        // The transfer is still owned by libusb. Leaking its small
                        // allocation is safer than freeing memory libusb may use.
                        std::mem::forget(queued);
                        return Err(UsbError::Timeout);
                    }
                    if self.context.handle_events(Some(remaining)).is_err() {
                        std::mem::forget(queued);
                        return Err(UsbError::Other);
                    }
                }
            }
            unsafe { rusb::ffi::libusb_free_transfer(queued.transfer) };
        }
        Ok(())
    }
    fn close(&mut self) -> Result<(), UsbError> {
        self.cancel_queued_bulk_read()?;
        if self.claimed {
            self.handle.release_interface(0).map_err(map_usb_error)?;
            self.claimed = false;
        }
        Ok(())
    }
}

impl Drop for RusbTransport {
    fn drop(&mut self) {
        let _ = self.cancel_queued_bulk_read();
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
    header_receive_queued: bool,
    queued_data_receives: u8,
    queued_data_receive_bytes: usize,
    device_block_carry: Vec<u8>,
    bytes_remaining: Option<usize>,
    bit_position: u64,
}

/// Production DSLogic source node using the `rusb` transport.
pub type DsLogicU3Pro16Source = LogicAnalyzerSource<DsLogicU3Pro16<RusbTransport>>;

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
            header_receive_queued: false,
            queued_data_receives: 0,
            queued_data_receive_bytes: 0,
            device_block_carry: Vec::new(),
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
        self.configure_fpga_once(image)
    }

    fn configure_fpga_once(&mut self, image: &[u8]) -> LogicAnalyzerResult<()> {
        tracing::debug!(image_bytes = image.len(), "starting FPGA configuration");
        self.command_write(3, 0, &[0xfb])?;
        self.command_write(5, 0, &[0xfc])?;
        self.poll_status_clear(0x20)?;
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
        self.bulk_write_exact(image, BULK_TIMEOUT)?;
        tracing::debug!(image_bytes = image.len(), "FPGA bitstream uploaded");
        // A completed host-side bulk transfer can precede the device-side
        // GPIF/FIFO drain. Raising INTRDY immediately is intermittent on the
        // U3Pro16 and can terminate configuration before the FPGA consumes
        // the end of the bitstream, leaving status at 0xab until power-cycle.
        thread::sleep(FPGA_UPLOAD_SETTLE);
        self.command_write(6, 0, &[0x80])?;
        self.poll_status(0x80)?;
        self.command_write(6, 0, &[0x7f])?;
        // Some U3Pro16 firmware revisions leave the configured-status bit
        // clear after a successful upload. The logic-version register is the
        // authoritative compatibility check in that case.
        let configuration_error = if let Err(status_error) =
            self.poll_status_for(0x40, FPGA_DONE_TIMEOUT)
        {
            let logic_version = self.logic_version()?;
            if logic_version != 0x0e {
                tracing::debug!(
                    logic_version = format_args!("{logic_version:#04x}"),
                    %status_error,
                    "FPGA configuration did not complete"
                );
                Some(LogicAnalyzerError::Protocol(format!(
                    "FPGA did not configure (logic version {logic_version:#04x}): {status_error}"
                )))
            } else {
                tracing::warn!(%status_error, "FPGA configured despite a clear configured-status bit");
                None
            }
        } else {
            None
        };
        // Restore the GPIF data path even when configuration failed. The
        // reference driver does this before returning, and without it a
        // subsequent configuration attempt can remain stuck at status 0xab.
        self.command_write(7, 0, &[1])?;
        if let Some(error) = configuration_error {
            return Err(error);
        }
        self.command_write(5, 0, &[0x01])?;
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
    /// The runtime exposes FPGA registers through the command-15 status
    /// block. The HDL version is byte four of a read beginning at offset zero;
    /// it is not addressable as an independent one-byte read.
    fn logic_version(&mut self) -> LogicAnalyzerResult<u8> {
        Ok(self.command_read(15, 0, 5)?[4])
    }

    fn ready_logic_version(&mut self) -> LogicAnalyzerResult<u8> {
        // The reference driver deasserts the FPGA acquisition clear before
        // reading the HDL status block. Without this write a configured
        // U3Pro16 can transiently return an all-zero block, which must not be
        // mistaken for an incompatible image and trigger a destructive reload.
        self.command_write(14, 0x70, &[0])?;
        let mut version = 0;
        for attempt in 1..=3 {
            version = self.logic_version()?;
            if version != 0 {
                break;
            }
            tracing::debug!(attempt, "U3Pro16 HDL version read returned zero; retrying");
            thread::sleep(Duration::from_millis(20));
        }
        Ok(version)
    }

    fn configure_internal_clock(&mut self) -> LogicAnalyzerResult<()> {
        // The U3Pro16 sample clock is supplied by an ADF4360 synthesizer. Its
        // configuration is volatile, so a freshly power-cycled device can
        // configure and arm the FPGA successfully but never produce samples
        // until this sequence has been written.
        self.command_write(14, ADC_CONTROL_ADDRESS + 2, &[0x01])?;
        for value in [0x01, 0x61, 0x00, 0x30] {
            self.command_write(14, ADC_CONTROL_ADDRESS, &[value])?;
        }
        for value in [0x01, 0x40, 0xf1, 0x46] {
            self.command_write(14, ADC_CONTROL_ADDRESS, &[value])?;
        }
        thread::sleep(Duration::from_millis(10));
        for value in [0x01, 0x62, 0x3d, 0x40] {
            self.command_write(14, ADC_CONTROL_ADDRESS, &[value])?;
        }
        tracing::debug!("configured U3Pro16 internal 500 MHz clock");
        Ok(())
    }

    fn poll_status(&mut self, required: u8) -> LogicAnalyzerResult<u8> {
        self.poll_status_for(required, STATUS_TIMEOUT)
    }

    fn poll_status_clear(&mut self, forbidden: u8) -> LogicAnalyzerResult<u8> {
        let until = Instant::now() + STATUS_TIMEOUT;
        let mut status = 0;
        while Instant::now() < until {
            status = self.command_read_byte(2, 0)?;
            if status & forbidden == 0 {
                tracing::debug!(
                    cleared_status_bits = format_args!("{forbidden:#04x}"),
                    status = format_args!("{status:#04x}"),
                    "device status cleared expected state"
                );
                return Ok(status);
            }
        }
        tracing::debug!(
            uncleared_status_bits = format_args!("{forbidden:#04x}"),
            final_status = format_args!("{status:#04x}"),
            "device status clear timed out"
        );
        Err(LogicAnalyzerError::Timeout(format!(
            "status bit(s) {forbidden:#04x} to clear; final status {status:#04x}"
        )))
    }

    fn poll_status_for(&mut self, required: u8, timeout: Duration) -> LogicAnalyzerResult<u8> {
        let until = Instant::now() + timeout;
        let mut status = 0;
        while Instant::now() < until {
            status = self.command_read_byte(2, 0)?;
            if status & required == required {
                tracing::debug!(
                    required_status_bits = format_args!("{required:#04x}"),
                    status = format_args!("{status:#04x}"),
                    "device status reached expected state"
                );
                return Ok(status);
            }
        }
        tracing::debug!(
            required_status_bits = format_args!("{required:#04x}"),
            final_status = format_args!("{status:#04x}"),
            "device status timed out"
        );
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

    /// The FPGA loader treats the configured byte count as one bulk-transfer
    /// transaction. A short write terminates that transaction; retrying the
    /// remainder as a second transfer can leave the FPGA unconfigured.
    fn bulk_write_exact(&mut self, data: &[u8], timeout: Duration) -> LogicAnalyzerResult<()> {
        let written = usb(
            self.transport.bulk_write(BULK_OUT, data, timeout),
            "FPGA bulk write",
        )?;
        if written != data.len() {
            return Err(LogicAnalyzerError::Protocol(format!(
                "short FPGA bulk write: {written}/{} bytes",
                data.len()
            )));
        }
        Ok(())
    }

    /// Ensure the capture FPGA is ready without exposing image management to
    /// callers. A normal runtime device is already configured. After power-up
    /// or a reset, the driver looks for the exact image in the environment or
    /// well-known local locations.
    fn ensure_fpga_configured(&mut self) -> LogicAnalyzerResult<()> {
        let status = self.command_read_byte(2, 0)?;
        let firmware = self.command_read(0, 0, 2)?;
        if status & 0x40 != 0 {
            let logic_version = self.ready_logic_version()?;
            tracing::debug!(
                status = format_args!("{status:#04x}"),
                logic_version = format_args!("{logic_version:#04x}"),
                firmware_major = firmware[0],
                firmware_minor = firmware[1],
                "checked FPGA state"
            );
            if logic_version == 0x0e {
                return Ok(());
            }
        } else {
            tracing::debug!(
                status = format_args!("{status:#04x}"),
                firmware_major = firmware[0],
                firmware_minor = firmware[1],
                "checked FPGA state"
            );
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

    fn configure_device_capture(&mut self, plan: DsLogicCapturePlan) -> LogicAnalyzerResult<()> {
        self.ensure_fpga_configured()?;
        if self.ready_logic_version()? != 0x0e {
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
        if !self.settings.external_clock {
            self.configure_internal_clock()?;
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

    fn poll_trigger_header(&mut self, plan: DsLogicCapturePlan) -> LogicAnalyzerResult<bool> {
        if !self.header_pending {
            return Ok(true);
        }

        let header = if self.header_receive_queued {
            let timeout = if self.settings.mode == CaptureMode::Streaming {
                Duration::ZERO
            } else {
                BULK_TIMEOUT
            };
            match self.transport.take_queued_bulk_read(1024, timeout) {
                Ok(Some(header)) => header,
                Ok(None) => {
                    tracing::debug!(
                        "queued trigger header was unavailable; falling back to a synchronous receive"
                    );
                    self.header_receive_queued = false;
                    let mut header = vec![0; 1024];
                    let read = match self.transport.bulk_read(BULK_IN, &mut header, BULK_TIMEOUT) {
                        Ok(read) => read,
                        Err(UsbError::Timeout) => return Ok(false),
                        Err(UsbError::Other) => {
                            return Err(LogicAnalyzerError::Transport(
                                "trigger header read".into(),
                            ));
                        }
                    };
                    header.truncate(read);
                    header
                }
                Err(UsbError::Timeout) => return Ok(false),
                Err(UsbError::Other) => {
                    return Err(LogicAnalyzerError::Transport(
                        "queued trigger header read".into(),
                    ));
                }
            }
        } else {
            let mut header = vec![0; 1024];
            let read = match self.transport.bulk_read(BULK_IN, &mut header, BULK_TIMEOUT) {
                Ok(read) => read,
                Err(UsbError::Timeout) => return Ok(false),
                Err(UsbError::Other) => {
                    return Err(LogicAnalyzerError::Transport("trigger header read".into()));
                }
            };
            header.truncate(read);
            header
        };

        self.header_receive_queued = false;
        if header.len() != 1024 {
            return Err(LogicAnalyzerError::Protocol(format!(
                "short trigger header: {}/1024 bytes",
                header.len()
            )));
        }
        tracing::debug!(
            prefix = ?&header[..16],
            "received U3Pro16 trigger header"
        );
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
        Ok(true)
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
        let header_receive_queued = self
            .transport
            .queue_bulk_read(BULK_IN, 1024, BULK_TIMEOUT)
            .map_err(|_| LogicAnalyzerError::Transport("queue trigger header read".into()))?;
        let initial_data_receive_bytes = match self.settings.mode {
            CaptureMode::Finite => plan.actual_bytes.min(1_048_576),
            CaptureMode::Streaming => plan.stream_buffer,
        };
        let requested_data_receives = match self.settings.mode {
            CaptureMode::Finite => 1,
            CaptureMode::Streaming => STREAMING_DATA_RECEIVE_DEPTH,
        };
        let mut queued_data_receives = 0;
        for _ in 0..requested_data_receives {
            match self
                .transport
                .queue_bulk_read(BULK_IN, initial_data_receive_bytes, BULK_TIMEOUT)
            {
                Ok(true) => queued_data_receives += 1,
                Ok(false) => break,
                Err(_) => {
                    let _ = self.transport.cancel_queued_bulk_read();
                    return Err(LogicAnalyzerError::Transport(
                        "queue initial logic data read".into(),
                    ));
                }
            }
        }
        tracing::debug!(
            header_receive_queued,
            "prepared U3Pro16 trigger header receive"
        );
        tracing::debug!(
            queued_data_receives,
            initial_data_receive_bytes,
            "prepared U3Pro16 initial logic data receive"
        );
        if let Err(error) = self.command_write(8, 0, &[]) {
            let _ = self.transport.cancel_queued_bulk_read();
            return Err(error);
        }
        self.plan = Some(plan);
        self.active = true;
        self.prepared = false;
        self.header_pending = true;
        self.header_receive_queued = header_receive_queued;
        self.queued_data_receives = queued_data_receives;
        self.queued_data_receive_bytes = initial_data_receive_bytes;
        self.device_block_carry.clear();
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
        let header_ready = self.poll_trigger_header(plan)?;
        if !header_ready && self.settings.mode == CaptureMode::Finite {
            return Ok(Some(self.empty_chunk(plan)));
        }
        if let Some(0) = self.bytes_remaining {
            self.active = false;
            return Ok(None);
        }
        let buffer_len = match self.bytes_remaining {
            Some(left) => left.min(1_048_576),
            None => plan.stream_buffer,
        };
        let timeout = if self.settings.mode == CaptureMode::Finite {
            Duration::from_millis(20)
        } else {
            Duration::from_millis(125)
        };
        let mut consumed_queued_data = false;
        let mut data = if self.queued_data_receives > 0 {
            match self
                .transport
                .take_queued_bulk_read(self.queued_data_receive_bytes, timeout)
            {
                Ok(Some(data)) => {
                    self.queued_data_receives -= 1;
                    consumed_queued_data = true;
                    data
                }
                Ok(None) => {
                    self.queued_data_receives = 0;
                    Vec::new()
                }
                Err(UsbError::Timeout) if self.settings.mode == CaptureMode::Streaming => {
                    self.check_streaming_integrity()?;
                    return Ok(Some(self.empty_chunk(plan)));
                }
                Err(error) => {
                    return Err(match error {
                        UsbError::Timeout => LogicAnalyzerError::Timeout("logic data read".into()),
                        UsbError::Other => {
                            LogicAnalyzerError::Transport("queued logic data read".into())
                        }
                    });
                }
            }
        } else {
            Vec::new()
        };
        if data.is_empty() {
            data.resize(buffer_len, 0);
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
            data.truncate(read);
        }
        let read = data.len();
        if read == 0 {
            if self.settings.mode == CaptureMode::Streaming {
                self.check_streaming_integrity()?;
                return Ok(Some(self.empty_chunk(plan)));
            }
            return Ok(None);
        }
        if consumed_queued_data && self.settings.mode == CaptureMode::Streaming {
            match self
                .transport
                .queue_bulk_read(BULK_IN, plan.stream_buffer, BULK_TIMEOUT)
            {
                Ok(true) => self.queued_data_receives += 1,
                Ok(false) => {}
                Err(_) => {
                    return Err(LogicAnalyzerError::Transport(
                        "queue replacement logic data read".into(),
                    ));
                }
            }
        }
        data.truncate(read);
        if let Some(left) = self.bytes_remaining.as_mut() {
            *left = left.checked_sub(read).ok_or_else(|| {
                LogicAnalyzerError::Protocol("received more than requested capture data".into())
            })?;
        }
        let data = canonicalize_cross_blocks(
            &mut self.device_block_carry,
            &data,
            usize::from(plan.channels),
        )?;
        let read = data.len();
        if read == 0 {
            return Ok(Some(self.empty_chunk(plan)));
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
        self.transport
            .cancel_queued_bulk_read()
            .map_err(|_| LogicAnalyzerError::Transport("cancel trigger header read".into()))?;
        self.header_receive_queued = false;
        self.queued_data_receives = 0;
        self.queued_data_receive_bytes = 0;
        self.device_block_carry.clear();
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

fn canonicalize_cross_blocks(
    carry: &mut Vec<u8>,
    incoming: &[u8],
    channel_count: usize,
) -> LogicAnalyzerResult<Vec<u8>> {
    let block_bytes = channel_count
        .checked_mul(8)
        .ok_or_else(|| LogicAnalyzerError::Protocol("cross-data block size overflow".into()))?;
    if channel_count == 0 || channel_count > 16 {
        return Err(LogicAnalyzerError::Protocol(format!(
            "invalid cross-data channel count {channel_count}"
        )));
    }
    carry.extend_from_slice(incoming);
    let complete_bytes = carry.len() / block_bytes * block_bytes;
    if complete_bytes == 0 {
        return Ok(Vec::new());
    }

    let mut output = Vec::with_capacity(complete_bytes);
    let mut accumulator = 0_u64;
    let mut accumulator_bits = 0_u32;
    for block in carry[..complete_bytes].chunks_exact(block_bytes) {
        for sample_byte in 0..8 {
            let mut transposed_groups = [0_u64; 2];
            for (group, transposed) in transposed_groups
                .iter_mut()
                .enumerate()
                .take(channel_count.div_ceil(8))
            {
                let mut rows = 0_u64;
                for row in 0..8 {
                    let channel = group * 8 + row;
                    if channel < channel_count {
                        rows |= u64::from(block[channel * 8 + sample_byte]) << (row * 8);
                    }
                }
                *transposed = transpose_8x8(rows);
            }
            for sample in 0..8 {
                for (group, transposed) in transposed_groups
                    .iter()
                    .enumerate()
                    .take(channel_count.div_ceil(8))
                {
                    let width = (channel_count - group * 8).min(8) as u32;
                    let value = (transposed >> (sample * 8)) & ((1_u64 << width) - 1);
                    accumulator |= value << accumulator_bits;
                    accumulator_bits += width;
                    while accumulator_bits >= 8 {
                        output.push(accumulator as u8);
                        accumulator >>= 8;
                        accumulator_bits -= 8;
                    }
                }
            }
        }
    }
    debug_assert_eq!(accumulator_bits, 0);
    let remainder = carry.split_off(complete_bytes);
    *carry = remainder;
    Ok(output)
}

#[inline]
fn transpose_8x8(mut value: u64) -> u64 {
    let mut swap = (value ^ (value >> 7)) & 0x00aa_00aa_00aa_00aa;
    value ^= swap ^ (swap << 7);
    swap = (value ^ (value >> 14)) & 0x0000_cccc_0000_cccc;
    value ^= swap ^ (swap << 14);
    swap = (value ^ (value >> 28)) & 0x0000_0000_f0f0_f0f0;
    value ^ swap ^ (swap << 28)
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
            usize::try_from(bytes_per_ms.div_ceil(STREAMING_TRANSFER_PARTS_PER_MS))
            .map_err(|_| {
                LogicAnalyzerError::InvalidSettings("stream buffer is too large".into())
            })?,
            1024,
        ),
        LinkSpeed::High => round_up_usize(
            usize::try_from(bytes_per_ms.div_ceil(STREAMING_TRANSFER_PARTS_PER_MS))
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
    _speed: LinkSpeed,
    settings: &DsLogicCaptureSettings,
    plan: DsLogicCapturePlan,
) -> LogicAnalyzerResult<[u8; 672]> {
    // The selectable maximum of a channel mode is its USB/data-path limit,
    // not the clock that feeds the FPGA divider. Every U3Pro16 logic mode
    // divides the fixed 500 MHz hardware clock; the 500 MHz and 1 GHz mode
    // flags below select their dedicated high-rate paths.
    let d0 = div_ceil(FPGA_DIVIDER_CLOCK_HZ, settings.sample_rate_hz)?;
    let mut divider_high = ((d0.min(FPGA_PRE_DIVIDER) - 1) << 8) as u16;
    let d = div_ceil(d0, FPGA_PRE_DIVIDER)?;
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
    (mask, value, edge, trigger_logic_word(stage), stage.count)
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
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use serde::Deserialize;

    use signal_processing::{
        CaptureAcquisitionPhase, CaptureCursorItem, CaptureEvent, CaptureFailureKind, CaptureIndex,
        CaptureQueueReceiveError, CaptureSessionId, CaptureStoreCursor, CaptureStoreDescriptor,
        NativeCaptureStore, NativeCaptureStoreConfig, bounded_capture_event_queue,
    };

    use crate::live_capture::{
        AcquisitionContext, DsLogicU3Pro16BufferedProvider, DsLogicU3Pro16StreamingProvider,
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

    fn encode_cross_blocks(canonical: &[u8], channel_count: usize) -> Vec<u8> {
        let bit_count = canonical.len() * 8;
        assert!(bit_count.is_multiple_of(channel_count * 64));
        let sample_count = bit_count / channel_count;
        let mut encoded = vec![0_u8; canonical.len()];
        for sample in 0..sample_count {
            for channel in 0..channel_count {
                let canonical_bit = sample * channel_count + channel;
                if canonical[canonical_bit / 8] & (1 << (canonical_bit % 8)) != 0 {
                    let block = sample / 64;
                    let block_sample = sample % 64;
                    let encoded_bit = block * channel_count * 64 + channel * 64 + block_sample;
                    encoded[encoded_bit / 8] |= 1 << (encoded_bit % 8);
                }
            }
        }
        encoded
    }

    struct FixtureTransport {
        control_reads: VecDeque<Vec<u8>>,
        control_writes: Arc<Mutex<Vec<Vec<u8>>>>,
        bulk_reads: VecDeque<Vec<u8>>,
        bulk_writes: Arc<Mutex<Vec<Vec<u8>>>>,
        idle_stream: bool,
        short_bulk_write: bool,
    }

    impl FixtureTransport {
        fn control_reads() -> VecDeque<Vec<u8>> {
            VecDeque::from([
                vec![0x40],
                vec![2, 0],
                vec![0, 0, 0, 0, 0x0e],
                vec![0, 0, 0, 0, 0x0e],
                vec![2, 0],
                vec![0x08],
                vec![0x80],
            ])
        }

        fn new(header: Vec<u8>, data: Vec<u8>) -> (Self, Arc<Mutex<Vec<Vec<u8>>>>) {
            const UNALIGNED_TRANSFER_BYTES: usize = 257;

            assert!(data.len() > UNALIGNED_TRANSFER_BYTES);
            let data = encode_cross_blocks(&data, 3);
            let bulk_writes = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    control_reads: Self::control_reads(),
                    control_writes: Arc::new(Mutex::new(Vec::new())),
                    bulk_reads: VecDeque::from([
                        header,
                        data[..UNALIGNED_TRANSFER_BYTES].to_vec(),
                        data[UNALIGNED_TRANSFER_BYTES..].to_vec(),
                    ]),
                    bulk_writes: Arc::clone(&bulk_writes),
                    idle_stream: false,
                    short_bulk_write: false,
                },
                bulk_writes,
            )
        }

        fn overflowing_stream(header: Vec<u8>) -> (Self, Arc<Mutex<Vec<Vec<u8>>>>) {
            let bulk_writes = Arc::new(Mutex::new(Vec::new()));
            let mut control_reads = Self::control_reads();
            control_reads.push_back(vec![0x10]);
            (
                Self {
                    control_reads,
                    control_writes: Arc::new(Mutex::new(Vec::new())),
                    bulk_reads: VecDeque::from([header]),
                    bulk_writes: Arc::clone(&bulk_writes),
                    idle_stream: false,
                    short_bulk_write: false,
                },
                bulk_writes,
            )
        }

        fn idle_stream(header: Vec<u8>) -> Self {
            Self {
                control_reads: Self::control_reads(),
                control_writes: Arc::new(Mutex::new(Vec::new())),
                bulk_reads: VecDeque::from([header]),
                bulk_writes: Arc::new(Mutex::new(Vec::new())),
                idle_stream: true,
                short_bulk_write: false,
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
            self.control_writes.lock().unwrap().push(data.to_vec());
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
            Ok(if self.short_bulk_write && !data.is_empty() {
                data.len() - 1
            } else {
                data.len()
            })
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

    struct PrequeuedHeaderTransport {
        inner: FixtureTransport,
        header: Option<Vec<u8>>,
        header_queued: bool,
    }

    impl PrequeuedHeaderTransport {
        fn new(header: Vec<u8>, data: Vec<u8>) -> Self {
            let (mut inner, _) = FixtureTransport::new(header, data);
            let header = inner.bulk_reads.pop_front();
            Self {
                inner,
                header,
                header_queued: false,
            }
        }
    }

    impl UsbTransport for PrequeuedHeaderTransport {
        fn link_speed(&self) -> LinkSpeed {
            self.inner.link_speed()
        }

        fn control_write(
            &mut self,
            request_type: u8,
            request: u8,
            value: u16,
            index: u16,
            data: &[u8],
            timeout: Duration,
        ) -> Result<usize, UsbError> {
            if data.first() == Some(&8) && !self.header_queued {
                return Err(UsbError::Other);
            }
            self.inner
                .control_write(request_type, request, value, index, data, timeout)
        }

        fn control_read(
            &mut self,
            request_type: u8,
            request: u8,
            value: u16,
            index: u16,
            data: &mut [u8],
            timeout: Duration,
        ) -> Result<usize, UsbError> {
            self.inner
                .control_read(request_type, request, value, index, data, timeout)
        }

        fn bulk_write(
            &mut self,
            endpoint: u8,
            data: &[u8],
            timeout: Duration,
        ) -> Result<usize, UsbError> {
            self.inner.bulk_write(endpoint, data, timeout)
        }

        fn bulk_read(
            &mut self,
            endpoint: u8,
            data: &mut [u8],
            timeout: Duration,
        ) -> Result<usize, UsbError> {
            self.inner.bulk_read(endpoint, data, timeout)
        }

        fn queue_bulk_read(
            &mut self,
            endpoint: u8,
            byte_len: usize,
            _timeout: Duration,
        ) -> Result<bool, UsbError> {
            if self.header_queued {
                return Ok(false);
            }
            if endpoint != BULK_IN || byte_len != 1024 || self.header.is_none() {
                return Err(UsbError::Other);
            }
            self.header_queued = true;
            Ok(true)
        }

        fn take_queued_bulk_read(
            &mut self,
            _byte_len: usize,
            _timeout: Duration,
        ) -> Result<Option<Vec<u8>>, UsbError> {
            if !self.header_queued {
                return Ok(None);
            }
            self.header_queued = false;
            Ok(self.header.take())
        }

        fn cancel_queued_bulk_read(&mut self) -> Result<(), UsbError> {
            self.header_queued = false;
            Ok(())
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

        let header =
            translate_trigger_header(&fixture_header(&fixture), CaptureMode::Finite, plan).unwrap();
        assert_eq!(
            header.trigger_sample(),
            Some(u64::from(fixture.trigger_sample))
        );
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
    fn cross_data_blocks_are_transposed_and_preserved_across_usb_boundaries() {
        let channel_count = 3;
        let mut canonical = vec![0_u8; channel_count * 8 * 2];
        for sample in 0..128 {
            for channel in 0..channel_count {
                if (sample + channel * 3).is_multiple_of(5 + channel) {
                    let bit = sample * channel_count + channel;
                    canonical[bit / 8] |= 1 << (bit % 8);
                }
            }
        }
        let encoded = encode_cross_blocks(&canonical, channel_count);
        let mut carry = Vec::new();

        let first = canonicalize_cross_blocks(&mut carry, &encoded[..17], channel_count).unwrap();
        let second = canonicalize_cross_blocks(&mut carry, &encoded[17..], channel_count).unwrap();

        assert!(first.is_empty());
        assert!(carry.is_empty());
        assert_eq!(second, canonical);
    }

    #[test]
    fn internal_clock_configuration_matches_the_u3pro16_500_mhz_sequence() {
        let control_writes = Arc::new(Mutex::new(Vec::new()));
        let transport = FixtureTransport {
            control_reads: VecDeque::new(),
            control_writes: Arc::clone(&control_writes),
            bulk_reads: VecDeque::new(),
            bulk_writes: Arc::new(Mutex::new(Vec::new())),
            idle_stream: false,
            short_bulk_write: false,
        };
        let mut analyzer = DsLogicU3Pro16::new(transport).unwrap();

        analyzer.configure_internal_clock().unwrap();

        let writes = control_writes.lock().unwrap();
        let register_writes = writes
            .iter()
            .map(|payload| {
                assert_eq!(payload[0], 14);
                assert_eq!(payload[3], 1);
                (u16::from_le_bytes([payload[1], payload[2]]), payload[4])
            })
            .collect::<Vec<_>>();
        assert_eq!(
            register_writes,
            vec![
                (0x4a, 0x01),
                (0x48, 0x01),
                (0x48, 0x61),
                (0x48, 0x00),
                (0x48, 0x30),
                (0x48, 0x01),
                (0x48, 0x40),
                (0x48, 0xf1),
                (0x48, 0x46),
                (0x48, 0x01),
                (0x48, 0x62),
                (0x48, 0x3d),
                (0x48, 0x40),
            ]
        );
    }

    #[test]
    fn fpga_configuration_rejects_a_short_bitstream_upload() {
        let transport = FixtureTransport {
            control_reads: VecDeque::from([vec![0x00], vec![0x20]]),
            control_writes: Arc::new(Mutex::new(Vec::new())),
            bulk_reads: VecDeque::new(),
            bulk_writes: Arc::new(Mutex::new(Vec::new())),
            idle_stream: false,
            short_bulk_write: true,
        };
        let mut analyzer = DsLogicU3Pro16::new(transport).unwrap();

        let error = analyzer.configure_fpga(&[0xaa, 0x55]).unwrap_err();

        assert!(matches!(
            error,
            LogicAnalyzerError::Protocol(message)
                if message == "short FPGA bulk write: 1/2 bytes"
        ));
    }

    #[test]
    fn fpga_configuration_allows_delayed_done_after_gpif_completion() {
        let mut control_reads = VecDeque::from([vec![0x00], vec![0x20], vec![0x80]]);
        control_reads.extend((0..55).map(|_| vec![0xab]));
        control_reads.push_back(vec![0xeb]);
        let transport = FixtureTransport {
            control_reads,
            control_writes: Arc::new(Mutex::new(Vec::new())),
            bulk_reads: VecDeque::new(),
            bulk_writes: Arc::new(Mutex::new(Vec::new())),
            idle_stream: false,
            short_bulk_write: false,
        };
        let mut analyzer = DsLogicU3Pro16::new(transport).unwrap();

        analyzer.configure_fpga(&[0xaa, 0x55]).unwrap();
    }

    #[test]
    fn configured_fpga_retries_a_transient_zero_hdl_version_without_reloading() {
        let bulk_writes = Arc::new(Mutex::new(Vec::new()));
        let transport = FixtureTransport {
            control_reads: VecDeque::from([
                vec![0x40],
                vec![2, 1],
                vec![0, 0, 0, 0, 0],
                vec![0, 0, 0, 0, 0],
                vec![0, 0, 0, 0, 0x0e],
            ]),
            control_writes: Arc::new(Mutex::new(Vec::new())),
            bulk_reads: VecDeque::new(),
            bulk_writes: Arc::clone(&bulk_writes),
            idle_stream: false,
            short_bulk_write: false,
        };
        let mut analyzer = DsLogicU3Pro16::new(transport).unwrap();

        analyzer.ensure_fpga_configured().unwrap();

        assert!(bulk_writes.lock().unwrap().is_empty());
    }

    #[test]
    #[ignore = "requires DSLOGIC_U3PRO16_FPGA_IMAGE and a connected DSLogic U3Pro16"]
    fn hardware_fpga_configuration_reaches_the_expected_logic_version() {
        let image_path = std::env::var_os("DSLOGIC_U3PRO16_FPGA_IMAGE")
            .expect("DSLOGIC_U3PRO16_FPGA_IMAGE must identify the exact U3Pro16 image");
        let image = std::fs::read(&image_path).unwrap();
        let mut analyzer = DsLogicU3Pro16::open_first().unwrap();

        analyzer.configure_fpga(&image).unwrap();
        assert_eq!(analyzer.logic_version().unwrap(), 0x0e);
    }

    #[test]
    #[ignore = "requires a connected DSLogic U3Pro16; captures 1,024 samples from inputs 0 and 1"]
    fn hardware_capture_receives_the_trigger_header_before_logic_data() {
        let config = LogicCaptureConfig::finite(1_000_000, 0b11, 1_024);
        let mut analyzer = DsLogicU3Pro16::open_first().unwrap();

        analyzer.configure_capture(&config).unwrap();
        analyzer.start_capture().unwrap();
        analyzer.next_chunk().unwrap();

        assert!(analyzer.take_trigger_header().is_some());
        analyzer.stop_capture().unwrap();
    }

    #[test]
    fn queues_the_trigger_header_receive_before_starting_capture() {
        let fixture = packet_fixture();
        let config = fixture_config(&fixture);
        let data = (0..768).map(|index| (index * 37) as u8).collect::<Vec<_>>();
        let transport = PrequeuedHeaderTransport::new(fixture_header(&fixture), data);
        let mut analyzer = DsLogicU3Pro16::new(transport).unwrap();

        analyzer.configure_capture(&config).unwrap();
        analyzer.start_capture().unwrap();
        analyzer.next_chunk().unwrap();

        assert!(analyzer.take_trigger_header().is_some());
    }

    #[test]
    fn streaming_polls_the_prequeued_trigger_header() {
        let fixture = packet_fixture();
        let mut config = fixture_config(&fixture);
        config.mode = CaptureMode::Streaming;
        let data = (0..768).map(|index| (index * 37) as u8).collect::<Vec<_>>();
        let transport = PrequeuedHeaderTransport::new(fixture_header(&fixture), data);
        let mut analyzer = DsLogicU3Pro16::new(transport).unwrap();

        analyzer.configure_capture(&config).unwrap();
        analyzer.start_capture().unwrap();
        analyzer.next_chunk().unwrap();

        assert_eq!(
            analyzer.take_trigger_header().and_then(|header| header.trigger_sample()),
            Some(u64::from(fixture.trigger_sample))
        );
    }

    #[test]
    fn buffered_provider_uploads_fixture_losslessly_and_publishes_actual_trigger() {
        let fixture = packet_fixture();
        let config = fixture_config(&fixture);
        let data = (0..768).map(|index| (index * 37) as u8).collect::<Vec<_>>();
        let expected_data = data.clone();
        let (transport, settings_writes) = FixtureTransport::new(fixture_header(&fixture), data);
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
                            let expected =
                                expected_data[source_bit / 8] & (1 << (source_bit % 8)) != 0;
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
        let transport = PrequeuedHeaderTransport::new(fixture_header(&fixture), data);
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
        let mut trigger_sample = None;
        loop {
            match event_reader.try_recv() {
                Ok(CaptureEvent::Status(status)) => phases.push(status.phase),
                Ok(CaptureEvent::Triggered { sample, .. }) => trigger_sample = Some(sample),
                Ok(
                    CaptureEvent::Progress { .. }
                    | CaptureEvent::Health { .. }
                    | CaptureEvent::Plan { .. },
                ) => {}
                Ok(CaptureEvent::Failed(failure)) => panic!("stream failed: {failure:?}"),
                Err(CaptureQueueReceiveError::Closed) => break,
                Err(CaptureQueueReceiveError::Empty) => std::thread::yield_now(),
                Err(CaptureQueueReceiveError::Timeout) => unreachable!(),
            }
        }
        assert_eq!(trigger_sample, Some(u64::from(fixture.trigger_sample)));
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
        let (_store, writer) =
            NativeCaptureStore::create(NativeCaptureStoreConfig::new(directory.path(), descriptor))
                .unwrap();
        let (events, event_reader) = bounded_capture_event_queue(64).unwrap();
        let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
        let mut acquisition = provider.prepare(context).unwrap();
        acquisition.start().unwrap();

        let error = acquisition.join().unwrap_err();

        assert!(matches!(
            error,
            crate::live_capture::AcquisitionError::Integrity(_)
        ));
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
        let (store, writer) =
            NativeCaptureStore::create(NativeCaptureStoreConfig::new(directory.path(), descriptor))
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
    fn wide_superspeed_stream_divides_the_fpga_clock_not_the_usb_mode_limit() {
        let settings = DsLogicCaptureSettings {
            mode: CaptureMode::Streaming,
            ..DsLogicCaptureSettings::finite(125_000_000, 0xffff, 125_000_000)
        };
        let plan = build_plan(LinkSpeed::Super, &settings).unwrap();
        let packet = build_settings_packet(LinkSpeed::Super, &settings, plan).unwrap();

        assert_eq!(get16(&packet, 10), 1);
        assert_eq!(get16(&packet, 12), 3 << 8);
    }

    #[test]
    #[ignore = "release-mode sustained-ingest benchmark; run with --release --ignored benchmark_streaming_ingest"]
    fn benchmark_streaming_ingest_store_summary_and_consumer_lag() {
        use signal_processing::NativeGrowingCaptureIndex;

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
                    signal_processing::CaptureChannelId::new(format!("u3pro16:input:{channel}"))
                })
                .collect::<Vec<_>>();
            let provider =
                DsLogicU3Pro16StreamingProvider::new(analyzer, config, channels.clone()).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let session_id = CaptureSessionId::new(0x9000 + channels_count as u128);
            let descriptor = CaptureStoreDescriptor::new(session_id, channels.clone()).unwrap();
            let (store, writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
                directory.path(),
                descriptor,
            ))
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
            let viewer_stop = Arc::new(AtomicBool::new(false));
            let viewer_stop_worker = Arc::clone(&viewer_stop);
            let mut viewer_index = index.clone();
            let viewer_channels = (0..channels_count).collect::<Vec<_>>();
            let viewer = std::thread::spawn(move || {
                while !viewer_stop_worker.load(Ordering::Relaxed) {
                    let total_samples = viewer_index.current_metadata().total_samples;
                    if total_samples > 0 {
                        let _ = viewer_index.sampled_window(
                            &viewer_channels,
                            0,
                            total_samples,
                            1_920,
                        );
                    }
                    std::thread::sleep(Duration::from_millis(8));
                }
            });
            let analyzed_samples = Arc::new(AtomicU64::new(0));
            let analyzed_samples_worker = Arc::clone(&analyzed_samples);
            let mut slow_cursor = store.open_cursor().unwrap();
            let slow_consumer = std::thread::spawn(move || {
                loop {
                    match slow_cursor.wait_next(Duration::from_millis(50)).unwrap() {
                        CaptureCursorItem::Chunk(chunk) => {
                            analyzed_samples_worker.store(chunk.end_sample(), Ordering::Relaxed);
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        CaptureCursorItem::Pending => {}
                        CaptureCursorItem::End => break,
                    }
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
            let summary_lag_at_finish = samples.saturating_sub(index.current_metadata().total_samples);
            viewer_stop.store(true, Ordering::Relaxed);
            viewer.join().unwrap();
            let summary_started = Instant::now();
            index_worker.join().unwrap();
            slow_consumer.join().unwrap();
            let catch_up_elapsed = summary_started.elapsed();
            store.finalize().unwrap();

            let mib = data_bytes as f64 / (1024.0 * 1024.0);
            eprintln!(
                "u3-stream channels={channels_count} rate_hz={rate_hz} samples={samples} data_mib={mib:.1} acquisition_s={:.3} ingest_mib_s={:.1} optional_consumer_lag_samples={lag_at_finish} summary_lag_samples={summary_lag_at_finish} summary_catchup_s={:.3} resident_summary_records={}",
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
