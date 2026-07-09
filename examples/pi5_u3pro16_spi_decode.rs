//! Capture the matching Pi 5 GPIO SPI test waveform with a DSLogic U3Pro16.

use dsl::nodes::decoders::{SpiDecoder, SpiMode, SpiTransfer};
use dsl::{
    CaptureMode, ClockSource, DsLogicU3Pro16, InputPort, LogicCaptureConfig, LogicEncodingRequest,
    LogicTrigger, LogicTriggerStage, OutputPort, Pipeline, PortDirection, PortSchema, ProcessNode,
    Sample, TriggerCondition, WorkError, WorkResult,
};
use std::collections::VecDeque;

const SAMPLE_RATE_HZ: u64 = 1_000_000;
const CAPTURE_SAMPLES: u64 = 5_000_000;
const THRESHOLD_VOLTS: f32 = 1.65;

struct Printer;
impl ProcessNode for Printer {
    fn name(&self) -> &str {
        "spi_printer"
    }
    fn num_inputs(&self) -> usize {
        1
    }
    fn num_outputs(&self) -> usize {
        0
    }
    fn input_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<SpiTransfer>(
            "transfers",
            0,
            PortDirection::Input,
        )]
    }
    fn work(&mut self, inputs: &[InputPort], _: &[OutputPort]) -> WorkResult<usize> {
        let mut buffer = VecDeque::new();
        let mut input = inputs
            .first()
            .and_then(|p| p.get::<SpiTransfer>(&mut buffer))
            .ok_or_else(|| WorkError::NodeError("missing SPI input".into()))?;
        let transfer = input.recv()?;
        let byte = transfer.mosi as u8;
        let text = match byte {
            b'\n' => "\\n".to_owned(),
            b'\r' => "\\r".to_owned(),
            b'\t' => "\\t".to_owned(),
            0x20..=0x7e => format!("{}", byte as char),
            _ => ".".to_owned(),
        };
        println!(
            "sample={} time={:.6}s MOSI=0x{:02X} text={:?}",
            transfer.timing.position,
            transfer.timing.timestamp_us / 1_000_000.0,
            byte,
            text
        );
        Ok(1)
    }
}

struct CsPrinter {
    previous: Option<bool>,
}
impl ProcessNode for CsPrinter {
    fn name(&self) -> &str {
        "cs_printer"
    }
    fn num_inputs(&self) -> usize {
        1
    }
    fn num_outputs(&self) -> usize {
        0
    }
    fn input_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<Sample>("cs", 0, PortDirection::Input)]
    }
    fn work(&mut self, inputs: &[InputPort], _: &[OutputPort]) -> WorkResult<usize> {
        let mut buffer = VecDeque::new();
        let mut input = inputs
            .first()
            .and_then(|p| p.get::<Sample>(&mut buffer))
            .ok_or_else(|| WorkError::NodeError("missing CS input".into()))?;
        let sample = input.recv()?;
        match self.previous {
            None => println!(
                "CS initial state: {}",
                if sample.value {
                    "released (HIGH)"
                } else {
                    "asserted (LOW)"
                }
            ),
            Some(true) if !sample.value => println!("CS asserted at {} ns", sample.start_time),
            Some(false) if sample.value => println!("CS released at {} ns", sample.start_time),
            _ => {}
        }
        self.previous = Some(sample.value);
        Ok(1)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let mut trigger_stage = LogicTriggerStage::default();
    trigger_stage.plane0[0] = TriggerCondition::Falling;
    let config = LogicCaptureConfig {
        mode: CaptureMode::Finite,
        sample_rate_hz: SAMPLE_RATE_HZ,
        input_mask: 0b111,
        sample_limit: CAPTURE_SAMPLES,
        trigger_percent: 10,
        threshold_volts: Some(THRESHOLD_VOLTS),
        trigger: LogicTrigger {
            stages: vec![trigger_stage],
            serial: false,
        },
        encoding: LogicEncodingRequest::Raw,
        clock: ClockSource::Internal,
        input_filter: false,
    };
    let analyzer = DsLogicU3Pro16::open_first()?;
    let mut pipeline = Pipeline::new().with_default_buffer_size(100_000);
    pipeline.add_process("source", analyzer.into_source(config)?)?;
    pipeline.add_process("spi", SpiDecoder::new(SpiMode::Mode0, 8, true, false))?;
    pipeline.add_process("printer", Printer)?;
    pipeline.add_process("cs_printer", CsPrinter { previous: None })?;
    pipeline.connect("source", "ch0", "spi", "cs")?;
    pipeline.connect("source", "ch0", "cs_printer", "cs")?;
    pipeline.connect("source", "ch1", "spi", "clk")?;
    pipeline.connect("source", "ch2", "spi", "mosi")?;
    pipeline.connect("spi", "spi_transfers", "printer", "transfers")?;
    pipeline.build()?.wait();
    Ok(())
}
