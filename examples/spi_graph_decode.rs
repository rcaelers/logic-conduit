//! Example: SPI-controlled parallel decode built from decomposed graph nodes
//!
//! Functionally the same pipeline as `spi_controlled_decode.rs`, but built
//! from the reusable primitives of `ANALYSIS_PIPELINE_DESIGN.md` Phase 1
//! instead of the fused `SpiCommandController`/`ControlledParallelWriter`:
//!
//! ```text
//! source ──► SPI decoder (24-bit) ──┬─► matcher(start) ──┬─► SR latch.set
//!                                   │                    └─► counter ─► formatter ─► writer.filename
//!                                   └─► matcher(stop) ───── SR latch.reset
//!                                                              │ q
//! source b{strobe,d0..d7,cs} ──► parallel decoder (DDR) ◄──────┘ enable
//!                                       │ words
//!                                       └─────────────────────► writer.data
//! ```
//!
//! Used as the Phase 1 golden diff against the old example: the
//! `capture_*.bin` outputs must be byte-identical (`captures.csv` matches
//! field-wise; the TGCK sidecar files are out of scope for the new writer).
//!
//! Usage (channels for the Epson V500 captures, e.g. _captures/wipneus5.dsl):
//!   cargo run --release --example spi_graph_decode -- \
//!       --file _captures/wipneus5.dsl \
//!       --spi-cs 8 --spi-clk 7 --spi-mosi 6 \
//!       --parallel-strobe 10 --parallel-data 0 1 2 3 4 5 6 7 \
//!       --enable-cmd 0x600081 --disable-cmd 0x600000 \
//!       --output-dir output_graph

use clap::Parser;
use dsl::DslFileSource;
use dsl::nodes::decoders::{
    CsPolarity, ParallelDecoder, SpiDecoder, SpiMode, SpiTransfer, StrobeMode,
};
use dsl::nodes::logic::{SrLatch, TextFormatter, TriggerCounter, WordMatcher};
use dsl::nodes::sinks::BinaryFileWriter;
use dsl::runtime::Pipeline;
use std::path::PathBuf;
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

    /// SPI MOSI channel
    #[arg(long)]
    spi_mosi: usize,

    /// Parallel strobe channel
    #[arg(long)]
    parallel_strobe: usize,

    /// Parallel data channels (in order)
    #[arg(long, num_args = 1..)]
    parallel_data: Vec<usize>,

    /// SPI command that enables the parallel decoder (hex)
    #[arg(long, value_parser = parse_hex)]
    enable_cmd: u64,

    /// SPI command that disables the parallel decoder (hex)
    #[arg(long, value_parser = parse_hex)]
    disable_cmd: u64,

    /// Output directory for captured data files
    #[arg(short, long, default_value = "output")]
    output_dir: PathBuf,
}

fn parse_hex(s: &str) -> Result<u64, std::num::ParseIntError> {
    let s = s.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(s, 16)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let max_channel = *[
        args.spi_cs,
        args.spi_clk,
        args.spi_mosi,
        args.parallel_strobe,
    ]
    .iter()
    .chain(args.parallel_data.iter())
    .max()
    .unwrap();
    let num_channels = (max_channel + 1) as u8;

    info!("File: {} ({} channels)", args.file, num_channels);
    info!(
        "Start command: 0x{:06X}, stop command: 0x{:06X}",
        args.enable_cmd, args.disable_cmd
    );

    let mut pipeline = Pipeline::new().with_default_buffer_size(10_000_000);

    // ── Nodes ────────────────────────────────────────────────────────────
    pipeline.add_process("source", DslFileSource::new(&args.file, num_channels)?)?;
    pipeline.add_process(
        "spi_decoder",
        SpiDecoder::new(SpiMode::Mode0, 24, true, false),
    )?;
    pipeline.add_process(
        "start_matcher",
        WordMatcher::<SpiTransfer>::new(args.enable_cmd, u64::MAX).with_name("start_matcher"),
    )?;
    pipeline.add_process(
        "stop_matcher",
        WordMatcher::<SpiTransfer>::new(args.disable_cmd, u64::MAX).with_name("stop_matcher"),
    )?;
    pipeline.add_process("latch", SrLatch::new(false))?;
    pipeline.add_process("counter", TriggerCounter::new(0, 1))?;
    pipeline.add_process(
        "formatter",
        TextFormatter::new(format!(
            "{}/capture_{{n:04}}.bin",
            args.output_dir.display()
        )),
    )?;
    pipeline.add_process(
        "parallel_decoder",
        ParallelDecoder::new(
            args.parallel_data.len(),
            StrobeMode::AnyEdge,
            CsPolarity::ActiveLow,
        ),
    )?;
    pipeline.add_process("writer", BinaryFileWriter::new().with_index_csv(true))?;

    // ── SPI control path ─────────────────────────────────────────────────
    pipeline.connect(
        "source",
        &format!("d{}", args.spi_clk),
        "spi_decoder",
        "clk",
    )?;
    pipeline.connect("source", &format!("d{}", args.spi_cs), "spi_decoder", "cs")?;
    pipeline.connect(
        "source",
        &format!("d{}", args.spi_mosi),
        "spi_decoder",
        "mosi",
    )?;
    pipeline.connect_with_buffer(
        "spi_decoder",
        "spi_transfers",
        "start_matcher",
        "words",
        1_000,
    )?;
    pipeline.connect_with_buffer(
        "spi_decoder",
        "spi_transfers",
        "stop_matcher",
        "words",
        1_000,
    )?;
    pipeline.connect_with_buffer("start_matcher", "trigger", "latch", "set", 100)?;
    pipeline.connect_with_buffer("stop_matcher", "trigger", "latch", "reset", 100)?;
    pipeline.connect_with_buffer("latch", "q", "parallel_decoder", "enable_signal", 100)?;

    // ── Filename path ────────────────────────────────────────────────────
    pipeline.connect_with_buffer("start_matcher", "trigger", "counter", "trigger", 100)?;
    pipeline.connect_with_buffer("counter", "count", "formatter", "value", 100)?;
    pipeline.connect_with_buffer("formatter", "text", "writer", "filename", 100)?;

    // ── Data path (block channels; each block ≈ 2 MB) ────────────────────
    pipeline.connect_with_buffer(
        "source",
        &format!("b{}", args.parallel_strobe),
        "parallel_decoder",
        "strobe",
        4,
    )?;
    for (i, &channel) in args.parallel_data.iter().enumerate() {
        pipeline.connect_with_buffer(
            "source",
            &format!("b{}", channel),
            "parallel_decoder",
            &format!("d{}", i),
            4,
        )?;
    }
    pipeline.connect_with_buffer(
        "source",
        &format!("b{}", args.spi_cs),
        "parallel_decoder",
        "cs",
        4,
    )?;
    pipeline.connect_with_buffer("parallel_decoder", "words", "writer", "data", 100_000)?;

    // ── Run ──────────────────────────────────────────────────────────────
    info!("Building pipeline...");
    let scheduler = pipeline.build()?;
    info!("Running...");
    scheduler.wait();
    info!("Done!");
    Ok(())
}
