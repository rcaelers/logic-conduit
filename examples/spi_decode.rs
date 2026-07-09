//! Example: Basic SPI decoding
//!
//! Demonstrates decoding SPI signals from a DSL file and printing the transfers.
//!
//! Usage:
//!   cargo run --release --example spi_decode -- \
//!       --file scan.dsl \
//!       --spi-cs 8 --spi-clk 7 --spi-mosi 6 \
//!       -n 100
//!
//! With CSV output:
//!   cargo run --release --example spi_decode -- \
//!       --file scan.dsl \
//!       --spi-cs 8 --spi-clk 7 --spi-mosi 6 \
//!       -n 100 \
//!       --csv-output output.csv

use clap::Parser;
use dsl::DslFileSource;
use dsl::nodes::decoders::{SpiDecoder, SpiMode};
use dsl::Word;
use dsl::runtime::{InputPort, OutputPort, Pipeline, ProcessNode, WorkError, WorkResult};
use std::fs::File;
use std::io::{BufWriter, Write};
use tracing::info;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to DSL file
    #[arg(short, long)]
    file: String,

    /// SPI chip select channel
    #[arg(long)]
    spi_cs: usize,

    /// SPI clock channel
    #[arg(long)]
    spi_clk: usize,

    /// SPI MOSI channel (command byte on channel 6)
    #[arg(long)]
    spi_mosi: usize,

    /// Number of SPI transfers to capture (0 = unlimited)
    #[arg(short, long, default_value = "100")]
    n: usize,

    /// CSV output file path (optional)
    #[arg(long)]
    csv_output: Option<String>,
}

/// Sink that prints SPI transfers
struct SpiPrinter {
    count: usize,
    max_transfers: usize,
}

impl SpiPrinter {
    fn new(max_transfers: usize) -> Self {
        Self {
            count: 0,
            max_transfers,
        }
    }
}

impl ProcessNode for SpiPrinter {
    fn name(&self) -> &str {
        "spi_printer"
    }

    fn should_stop(&self) -> bool {
        self.max_transfers > 0 && self.count >= self.max_transfers
    }

    fn num_inputs(&self) -> usize {
        1
    }

    fn num_outputs(&self) -> usize {
        0 // Sink
    }

    fn input_schema(&self) -> Vec<dsl::PortSchema> {
        use dsl::{PortDirection, PortSchema};
        vec![PortSchema::new::<Word>(
            "spi_transfers",
            0,
            PortDirection::Input,
        )]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut input_buffer = std::collections::VecDeque::new();
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<Word>(&mut input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing input channel".to_string()))?;

        let word = input.recv()?;

        self.count += 1;
        info!(
            "SPI Transfer #{}: MOSI=0x{:06X} at t={} ns",
            self.count, word.value, word.timestamp_ns
        );

        if self.max_transfers > 0 && self.count >= self.max_transfers {
            info!(
                "[SpiPrinter] Max transfers ({}) reached, shutting down",
                self.max_transfers
            );
            return Err(WorkError::Shutdown);
        }

        Ok(1)
    }
}

/// Sink that writes SPI transfers to CSV file
struct SpiCsvWriter {
    writer: BufWriter<File>,
    count: usize,
    max_transfers: usize,
}

impl SpiCsvWriter {
    fn new(path: &str, max_transfers: usize) -> Result<Self, std::io::Error> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        // Write CSV header
        writeln!(writer, "Id,Time[ns],1:SPI: MOSI data")?;

        Ok(Self {
            writer,
            count: 0,
            max_transfers,
        })
    }
}

impl ProcessNode for SpiCsvWriter {
    fn name(&self) -> &str {
        "spi_csv_writer"
    }

    fn should_stop(&self) -> bool {
        self.max_transfers > 0 && self.count >= self.max_transfers
    }

    fn num_inputs(&self) -> usize {
        1
    }

    fn num_outputs(&self) -> usize {
        0 // Sink
    }

    fn input_schema(&self) -> Vec<dsl::PortSchema> {
        use dsl::{PortDirection, PortSchema};
        vec![PortSchema::new::<Word>(
            "spi_transfers",
            0,
            PortDirection::Input,
        )]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut input_buffer = std::collections::VecDeque::new();
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<Word>(&mut input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing input channel".to_string()))?;

        let word = input.recv()?;

        self.count += 1;

        // Output raw MOSI value (all 24 bits from channel 6)
        writeln!(
            self.writer,
            "{},{:.2},{:06X}",
            self.count, word.timestamp_ns as f64, word.value
        )
        .map_err(|e| WorkError::NodeError(format!("CSV write error: {}", e)))?;

        if self.max_transfers > 0 && self.count >= self.max_transfers {
            // Flush before shutdown
            self.writer
                .flush()
                .map_err(|e| WorkError::NodeError(format!("CSV flush error: {}", e)))?;
            info!(
                "[SpiCsvWriter] Max transfers ({}) reached, shutting down",
                self.max_transfers
            );
            return Err(WorkError::Shutdown);
        }

        Ok(1)
    }
}

impl Drop for SpiCsvWriter {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    info!("=== SPI Decode Example ===");
    info!("File: {}", args.file);
    info!(
        "SPI: CS={}, CLK={}, MOSI={}",
        args.spi_cs, args.spi_clk, args.spi_mosi
    );

    // Calculate total channels needed
    let max_channel = *[args.spi_cs, args.spi_clk, args.spi_mosi]
        .iter()
        .max()
        .unwrap();

    let num_channels = (max_channel + 1) as u8;
    info!("Using {} channels", num_channels);

    // Create pipeline with large buffers
    let mut pipeline = Pipeline::new().with_default_buffer_size(10_000_000);

    // Add file source
    pipeline.add_process("source", DslFileSource::new(&args.file, num_channels)?)?;

    // Add SPI decoder (3 inputs: CLK, CS, MOSI; 1 output: transfers)
    // Mode0: CPOL=0, CPHA=0, active-low CS, MSB-first
    pipeline.add_process(
        "spi_decoder",
        SpiDecoder::new(
            SpiMode::Mode0, // CPOL=0, CPHA=0 (sample on rising edge)
            24,             // 24-bit words (8-bit command + 16-bit data)
            true,           // has_mosi
            false,          // no MISO - single wire
        ),
    )?;

    // Wire SPI decoder inputs from source
    pipeline.connect(
        "source",
        &format!("ch{}", args.spi_clk),
        "spi_decoder",
        "clk",
    )?;
    pipeline.connect(
        "source",
        &format!("ch{}", args.spi_cs),
        "spi_decoder",
        "cs",
    )?;
    pipeline.connect(
        "source",
        &format!("ch{}", args.spi_mosi),
        "spi_decoder",
        "mosi",
    )?;

    // Add SPI printer sink
    pipeline.add_process("printer", SpiPrinter::new(args.n))?;
    pipeline.connect("spi_decoder", "mosi_words", "printer", "spi_transfers")?;

    // Optionally add CSV writer
    if let Some(csv_path) = &args.csv_output {
        info!("CSV output: {}", csv_path);
        pipeline.add_process("csv_writer", SpiCsvWriter::new(csv_path, args.n)?)?;
        pipeline.connect(
            "spi_decoder",
            "mosi_words",
            "csv_writer",
            "spi_transfers",
        )?;
    }

    // Build and run
    info!("Building pipeline...");
    let scheduler = pipeline.build()?;

    info!("Running...");
    scheduler.wait();

    info!("Done!");

    Ok(())
}
