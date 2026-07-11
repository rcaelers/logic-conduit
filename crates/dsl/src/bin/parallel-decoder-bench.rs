use clap::{Parser, ValueEnum};
use dsl::runtime::{EdgeQuery, ProtocolKind};
use dsl::{
    CsPolarity, DerivedLaneData, DerivedLanes, DslFileSource, InputPort, OutputPort,
    ParallelDecoder, Pipeline, PortSchema, ProcessNode, StrobeMode, ViewerLaneKind, ViewerSink,
    Word, WorkError, WorkResult,
};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BenchMode {
    Indexed,
    Stream,
    Both,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SinkKind {
    Count,
    Viewer,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TriggerMode {
    Rising,
    Falling,
    Both,
}

impl From<TriggerMode> for StrobeMode {
    fn from(value: TriggerMode) -> Self {
        match value {
            TriggerMode::Rising => StrobeMode::RisingEdge,
            TriggerMode::Falling => StrobeMode::FallingEdge,
            TriggerMode::Both => StrobeMode::AnyEdge,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BenchCsPolarity {
    Disabled,
    ActiveLow,
    ActiveHigh,
}

impl From<BenchCsPolarity> for CsPolarity {
    fn from(value: BenchCsPolarity) -> Self {
        match value {
            BenchCsPolarity::Disabled => CsPolarity::Disabled,
            BenchCsPolarity::ActiveLow => CsPolarity::ActiveLow,
            BenchCsPolarity::ActiveHigh => CsPolarity::ActiveHigh,
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "Benchmark file-backed ParallelDecoder throughput")]
struct Args {
    /// DSLogic capture to decode.
    capture: PathBuf,

    /// Maximum number of capture samples to process.
    #[arg(long, default_value_t = 200_000_000)]
    samples: u64,

    /// File input protocol to benchmark.
    #[arg(long, value_enum, default_value_t = BenchMode::Both)]
    mode: BenchMode,

    /// Downstream sink included in the measurement.
    #[arg(long, value_enum, default_value_t = SinkKind::Count)]
    sink: SinkKind,

    /// Strobe channel number.
    #[arg(long, default_value_t = 10)]
    strobe: usize,

    /// Data channels in least-significant-bit first order.
    #[arg(long, value_delimiter = ',', default_value = "0,1,2,3,4,5,6,7")]
    data: Vec<usize>,

    /// Optional chip-select channel number.
    #[arg(long)]
    cs: Option<usize>,

    /// Chip-select polarity. Disabled does not require --cs.
    #[arg(long, value_enum, default_value_t = BenchCsPolarity::Disabled)]
    cs_polarity: BenchCsPolarity,

    /// Strobe edge selection.
    #[arg(long, value_enum, default_value_t = TriggerMode::Both)]
    trigger: TriggerMode,

    /// Crossbeam buffer capacity for each pipeline connection.
    #[arg(long, default_value_t = 1_000)]
    buffer: usize,
}

struct ForceStreamOutput<N>(N);

impl<N: ProcessNode> ProcessNode for ForceStreamOutput<N> {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn should_stop(&self) -> bool {
        self.0.should_stop()
    }

    fn is_self_threading(&self) -> bool {
        self.0.is_self_threading()
    }

    fn num_inputs(&self) -> usize {
        self.0.num_inputs()
    }

    fn num_outputs(&self) -> usize {
        self.0.num_outputs()
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        self.0.input_schema()
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        self.0
            .output_schema()
            .into_iter()
            .map(|schema| schema.with_protocols(vec![ProtocolKind::Stream]))
            .collect()
    }

    fn node_type(&self) -> &str {
        self.0.node_type()
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        self.0.work(inputs, outputs)
    }

    fn edge_query(
        &self,
        port: usize,
        input_queries: &[Option<Arc<dyn EdgeQuery>>],
    ) -> Option<Arc<dyn EdgeQuery>> {
        self.0.edge_query(port, input_queries)
    }
}

struct CountWords {
    count: Arc<AtomicU64>,
    buffer: VecDeque<Word>,
}

impl CountWords {
    const DRAIN_BATCH: usize = 4_096;

    fn new(count: Arc<AtomicU64>) -> Self {
        Self {
            count,
            buffer: VecDeque::new(),
        }
    }
}

impl ProcessNode for CountWords {
    fn name(&self) -> &str {
        "count_words"
    }

    fn num_inputs(&self) -> usize {
        1
    }

    fn num_outputs(&self) -> usize {
        0
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        use dsl::PortDirection;
        vec![PortSchema::new::<Word>("words", 0, PortDirection::Input)]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<Word>(&mut self.buffer))
            .ok_or_else(|| WorkError::NodeError("missing word input".to_string()))?;

        input.recv()?;
        let mut received = 1usize;
        while received < Self::DRAIN_BATCH && input.try_recv().is_ok() {
            received += 1;
        }
        self.count.fetch_add(received as u64, Ordering::Relaxed);
        Ok(received)
    }
}

struct BenchResult {
    mode: BenchMode,
    sink: SinkKind,
    setup: Duration,
    elapsed: Duration,
    samples: u64,
    samplerate_hz: f64,
    words: u64,
}

impl BenchResult {
    fn print(&self) {
        let seconds = self.elapsed.as_secs_f64();
        let capture_seconds = self.samples as f64 / self.samplerate_hz;
        let msamples_per_second = self.samples as f64 / seconds / 1_000_000.0;
        let mwords_per_second = self.words as f64 / seconds / 1_000_000.0;
        let realtime = capture_seconds / seconds;
        println!(
            "mode={:?} sink={:?} samples={} words={} setup_s={:.3} run_s={:.3} capture_s={:.3} MSamples_s={:.3} MWords_s={:.3} realtime_x={:.3}",
            self.mode,
            self.sink,
            self.samples,
            self.words,
            self.setup.as_secs_f64(),
            seconds,
            capture_seconds,
            msamples_per_second,
            mwords_per_second,
            realtime,
        );
    }
}

fn run(args: &Args, mode: BenchMode) -> Result<BenchResult, Box<dyn std::error::Error>> {
    let required_channels = args
        .data
        .iter()
        .copied()
        .chain([args.strobe])
        .chain(args.cs)
        .max()
        .map_or(0, |channel| channel + 1);
    let source = DslFileSource::new(&args.capture, required_channels as u8)?;
    let samples = args.samples.min(source.total_samples());
    let samplerate_hz = source.samplerate_hz();
    let source = source.with_max_samples(Some(samples));
    let cs_polarity = CsPolarity::from(args.cs_polarity);
    let decoder = ParallelDecoder::new(args.data.len(), args.trigger.into(), cs_polarity);

    let count = Arc::new(AtomicU64::new(0));
    let mut viewer_store = None;
    let sink_port;
    let mut pipeline = Pipeline::new().with_default_buffer_size(args.buffer);
    match mode {
        BenchMode::Indexed => pipeline.add_process("source", source)?,
        BenchMode::Stream => pipeline.add_process("source", ForceStreamOutput(source))?,
        BenchMode::Both => unreachable!("Both is expanded by main"),
    }
    pipeline.add_process("decoder", decoder)?;
    match args.sink {
        SinkKind::Count => {
            pipeline.add_process("sink", CountWords::new(Arc::clone(&count)))?;
            sink_port = "words";
        }
        SinkKind::Viewer => {
            let store = DerivedLanes::new();
            pipeline.add_process(
                "sink",
                ViewerSink::new(store.clone()).with_lane(ViewerLaneKind::Words, "parallel"),
            )?;
            viewer_store = Some(store);
            sink_port = "in0";
        }
    }

    pipeline.connect("source", &format!("ch{}", args.strobe), "decoder", "strobe")?;
    for (bit, channel) in args.data.iter().enumerate() {
        pipeline.connect(
            "source",
            &format!("ch{channel}"),
            "decoder",
            &format!("d{bit}"),
        )?;
    }
    if cs_polarity != CsPolarity::Disabled {
        let channel = args
            .cs
            .ok_or("--cs is required when chip select is enabled")?;
        pipeline.connect("source", &format!("ch{channel}"), "decoder", "cs")?;
    }
    pipeline.connect("decoder", "words", "sink", sink_port)?;

    let setup_start = Instant::now();
    let scheduler = pipeline.build()?;
    let setup = setup_start.elapsed();
    let run_start = Instant::now();
    scheduler.wait();
    let elapsed = run_start.elapsed();

    let words = if let Some(store) = viewer_store {
        let lanes = store.read();
        match lanes.first().map(|lane| &lane.data) {
            Some(DerivedLaneData::Annotations(words)) => words.len() as u64,
            _ => 0,
        }
    } else {
        count.load(Ordering::Relaxed)
    };

    Ok(BenchResult {
        mode,
        sink: args.sink,
        setup,
        elapsed,
        samples,
        samplerate_hz,
        words,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    if !args.capture.is_file() {
        return Err(format!("capture does not exist: {}", args.capture.display()).into());
    }
    if args.data.is_empty() || args.data.len() > 64 {
        return Err("--data must contain between 1 and 64 channels".into());
    }
    if args
        .data
        .iter()
        .copied()
        .chain([args.strobe])
        .chain(args.cs)
        .any(|channel| channel >= 16)
    {
        return Err("DSL channel numbers must be between 0 and 15".into());
    }

    let modes: &[BenchMode] = match args.mode {
        BenchMode::Indexed => &[BenchMode::Indexed],
        BenchMode::Stream => &[BenchMode::Stream],
        BenchMode::Both => &[BenchMode::Indexed, BenchMode::Stream],
    };
    for &mode in modes {
        run(&args, mode)?.print();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_capture_and_channel_mapping() {
        let args = Args::try_parse_from([
            "parallel-decoder-bench",
            "capture.dsl",
            "--samples",
            "12345",
            "--mode",
            "stream",
            "--sink",
            "viewer",
            "--strobe",
            "9",
            "--data",
            "2,4,6",
            "--cs",
            "8",
            "--cs-polarity",
            "active-low",
        ])
        .unwrap();

        assert_eq!(args.capture, PathBuf::from("capture.dsl"));
        assert_eq!(args.samples, 12_345);
        assert!(matches!(args.mode, BenchMode::Stream));
        assert!(matches!(args.sink, SinkKind::Viewer));
        assert_eq!(args.strobe, 9);
        assert_eq!(args.data, vec![2, 4, 6]);
        assert_eq!(args.cs, Some(8));
        assert!(matches!(args.cs_polarity, BenchCsPolarity::ActiveLow));
    }

    #[test]
    fn capture_path_is_required() {
        assert!(Args::try_parse_from(["parallel-decoder-bench"]).is_err());
    }
}
