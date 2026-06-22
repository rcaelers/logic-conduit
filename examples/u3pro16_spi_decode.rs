//! Capture and decode SPI directly from a DSLogic U3Pro16.
//!
//! Example:
//! ```text
//! cargo run --release --example u3pro16_spi_decode -- \
//!   --cs 8 --clk 7 --mosi 6 --sample-rate 100000000 --samples 1000000
//! ```
//!
//! Add `--fpga-image /path/to/DSLogicU3Pro16.bin` when the FPGA is not
//! already configured. The image must be the exact U3Pro16 image.

use clap::Parser;
use dsl::nodes::decoders::{SpiDecoder, SpiMode, SpiTransfer};
use dsl::{
    CaptureMode, ClockSource, DsLogicU3Pro16, InputPort, LogicCaptureConfig, LogicEncodingRequest,
    LogicTrigger, OutputPort, Pipeline, PortDirection, PortSchema, ProcessNode, WorkError,
    WorkResult,
};
use std::collections::VecDeque;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(about = "Capture SPI from a DSLogic U3Pro16")]
struct Args {
    /// Physical U3Pro16 input carrying active-low chip select.
    #[arg(long)]
    cs: u8,
    /// Physical U3Pro16 input carrying SPI clock.
    #[arg(long)]
    clk: u8,
    /// Physical U3Pro16 input carrying MOSI.
    #[arg(long)]
    mosi: u8,
    /// Capture rate in Hz; must be a hardware-supported discrete rate.
    #[arg(long, default_value_t = 100_000_000)]
    sample_rate: u64,
    /// Number of samples to capture per enabled input.
    #[arg(long, default_value_t = 1_000_000)]
    samples: u64,
    /// SPI word width.
    #[arg(long, default_value_t = 8)]
    bits_per_word: usize,
    /// Optional input threshold in volts.
    #[arg(long)]
    threshold: Option<f32>,
    /// Configure the FPGA from this exact U3Pro16 .bin image before capture.
    #[arg(long)]
    fpga_image: Option<PathBuf>,
}

struct SpiPrinter;

impl ProcessNode for SpiPrinter {
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
    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut buffer = VecDeque::new();
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<SpiTransfer>(&mut buffer))
            .ok_or_else(|| WorkError::NodeError("missing SPI transfer input".into()))?;
        let transfer = input.recv()?;
        println!(
            "t={:.6}s sample={} MOSI=0x{:X}",
            transfer.timing.timestamp_us / 1_000_000.0,
            transfer.timing.position,
            transfer.mosi,
        );
        Ok(1)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let highest_input = args.cs.max(args.clk).max(args.mosi);
    if highest_input >= 16 {
        return Err("U3Pro16 input numbers must be 0..15".into());
    }

    // Select all low-numbered inputs through the highest requested one. This
    // makes source port dN correspond directly to physical U3Pro16 input N.
    let config = LogicCaptureConfig {
        mode: CaptureMode::Finite,
        sample_rate_hz: args.sample_rate,
        input_mask: (1u64 << (u32::from(highest_input) + 1)) - 1,
        sample_limit: args.samples,
        trigger_percent: 50,
        threshold_volts: args.threshold,
        trigger: LogicTrigger::default(),
        encoding: LogicEncodingRequest::Raw,
        clock: ClockSource::Internal,
        input_filter: false,
    };

    let mut analyzer = DsLogicU3Pro16::open_first()?;
    if let Some(path) = args.fpga_image {
        analyzer.configure_fpga(&std::fs::read(path)?)?;
    }

    let mut pipeline = Pipeline::new().with_default_buffer_size(100_000);
    pipeline.add_process("source", analyzer.into_source(config)?)?;
    pipeline.add_process(
        "spi",
        SpiDecoder::new(SpiMode::Mode0, args.bits_per_word, true, false),
    )?;
    pipeline.add_process("printer", SpiPrinter)?;
    pipeline.connect("source", &format!("d{}", args.cs), "spi", "cs")?;
    pipeline.connect("source", &format!("d{}", args.clk), "spi", "clk")?;
    pipeline.connect("source", &format!("d{}", args.mosi), "spi", "mosi")?;
    pipeline.connect("spi", "spi_transfers", "printer", "transfers")?;
    pipeline.build()?.wait();
    Ok(())
}
