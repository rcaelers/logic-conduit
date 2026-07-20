//! Parallel decoder benchmark.

std::cfg_select! {
    target_arch = "wasm32" => {
        fn main() {}
    }
    _ => {
        fn main() -> Result<(), Box<dyn std::error::Error>> {
            native::main()
        }

        mod native {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use clap::{Parser, ValueEnum};

    use logic_analyzer_processing::{
        BinaryFileWriter, CsPolarity, DslFileSource, ParallelDecoder, ParallelInputStrategy,
        StrobeMode,
    };
    use signal_processing::{
        DecodedBlockCacheStats, DerivedLaneData, DerivedLanes, InputPort, LiveStoreConfig,
        OutputPort, PersistentStoreConfig, Pipeline, PortSchema, ProcessNode, ProtocolKind,
        ViewerLaneKind, ViewerRetention, ViewerSink, ViewerSinkMetrics, Word, WorkError,
        WorkResult, configure_decoded_block_cache, decoded_block_cache_stats,
        reset_decoded_block_cache_stats,
    };

    const DEFAULT_MAX_WORDS_PER_BLOCK: usize = 32_768;
    const DEFAULT_RESTART_INTERVAL: usize = 512;

    #[derive(Clone, Copy, Debug, ValueEnum)]
    enum BenchMode {
        Indexed,
        Stream,
        Auto,
        Both,
    }

    impl BenchMode {
        fn protocol_name(self) -> &'static str {
            match self {
                Self::Indexed => "edge-query",
                Self::Stream => "packed-stream",
                Self::Auto => "auto",
                Self::Both => "multiple",
            }
        }
    }

    #[derive(Clone, Copy, Debug, ValueEnum)]
    enum SinkKind {
        Discard,
        Count,
        Retain,
        Viewer,
        File,
    }

    #[derive(Clone, Copy, Debug, ValueEnum)]
    enum CacheKind {
        Temporary,
        Persistent,
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

        /// Packed fragment scans allowed concurrently (1-8).
        #[arg(long, default_value_t = 4)]
        workers: usize,

        /// Maximum retained annotations for the viewer sink.
        #[arg(long, default_value_t = 4_000_000)]
        viewer_max_entries: usize,

        /// Indexed viewer cache publication mode.
        #[arg(long, value_enum, default_value_t = CacheKind::Temporary)]
        cache: CacheKind,

        /// Shared decoded-word block cache budget used by query validation.
        #[arg(long, default_value_t = 64)]
        decoded_cache_mib: usize,

        /// Number of exact, presence, and cursor queries in post-run validation.
        #[arg(long, default_value_t = 200)]
        query_samples: usize,

        /// Maximum words per encoded derived-store block.
        #[arg(long, default_value_t = DEFAULT_MAX_WORDS_PER_BLOCK)]
        store_block_words: usize,

        /// Word interval between random-access restart records.
        #[arg(long, default_value_t = DEFAULT_RESTART_INTERVAL)]
        store_restart_interval: usize,
    }

    struct CountWords {
        stats: Arc<Mutex<OutputStats>>,
        buffer: VecDeque<Word>,
    }

    impl CountWords {
        const DRAIN_BATCH: usize = 65_536;

        fn new(stats: Arc<Mutex<OutputStats>>) -> Self {
            Self {
                stats,
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
            use signal_processing::PortDirection;
            vec![PortSchema::new::<Word>("words", 0, PortDirection::Input)]
        }

        fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            let mut input = inputs
                .first()
                .and_then(|port| port.get::<Word>(&mut self.buffer))
                .ok_or_else(|| WorkError::NodeError("missing word input".to_string()))?;

            let mut batch = Vec::with_capacity(Self::DRAIN_BATCH);
            let received = match input.try_recv_many(&mut batch, Self::DRAIN_BATCH) {
                Ok(received) => received,
                Err(crossbeam_channel::TryRecvError::Empty) => return Ok(0),
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    return Err(WorkError::Shutdown);
                }
            };
            self.stats.lock().unwrap().extend_words(&batch);
            Ok(received)
        }
    }

    struct RetainWords {
        words: Arc<Mutex<Vec<Word>>>,
        buffer: VecDeque<Word>,
    }

    impl RetainWords {
        fn new(words: Arc<Mutex<Vec<Word>>>) -> Self {
            Self {
                words,
                buffer: VecDeque::new(),
            }
        }
    }

    impl ProcessNode for RetainWords {
        fn name(&self) -> &str {
            "retain_words"
        }

        fn num_inputs(&self) -> usize {
            1
        }

        fn num_outputs(&self) -> usize {
            0
        }

        fn input_schema(&self) -> Vec<PortSchema> {
            use signal_processing::PortDirection;
            vec![PortSchema::new::<Word>("words", 0, PortDirection::Input)]
        }

        fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            let mut input = inputs
                .first()
                .and_then(|port| port.get::<Word>(&mut self.buffer))
                .ok_or_else(|| WorkError::NodeError("missing word input".to_string()))?;
            let mut batch = Vec::with_capacity(CountWords::DRAIN_BATCH);
            let received = match input.try_recv_many(&mut batch, CountWords::DRAIN_BATCH) {
                Ok(received) => received,
                Err(crossbeam_channel::TryRecvError::Empty) => return Ok(0),
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    return Err(WorkError::Shutdown);
                }
            };
            self.words.lock().unwrap().extend(batch);
            Ok(received)
        }
    }

    const FINGERPRINT_OFFSET: u64 = 0xcbf29ce484222325;
    const FINGERPRINT_PRIME: u64 = 0x100000001b3;

    fn fingerprint_u64(mut fingerprint: u64, value: u64) -> u64 {
        for byte in value.to_le_bytes() {
            fingerprint ^= u64::from(byte);
            fingerprint = fingerprint.wrapping_mul(FINGERPRINT_PRIME);
        }
        fingerprint
    }

    #[derive(Debug, Clone, Copy)]
    struct OutputStats {
        count: u64,
        fingerprint: u64,
    }

    impl Default for OutputStats {
        fn default() -> Self {
            Self {
                count: 0,
                fingerprint: FINGERPRINT_OFFSET,
            }
        }
    }

    impl OutputStats {
        fn extend_words(&mut self, words: &[Word]) {
            for word in words {
                self.fingerprint = fingerprint_u64(self.fingerprint, word.value);
                self.fingerprint = fingerprint_u64(self.fingerprint, word.timestamp_ns);
                self.fingerprint = fingerprint_u64(self.fingerprint, word.duration_ns);
            }
            self.count += words.len() as u64;
        }

        fn extend_annotations(&mut self, annotations: &[signal_processing::Annotation]) {
            for annotation in annotations {
                self.fingerprint = fingerprint_u64(self.fingerprint, annotation.value);
                self.fingerprint = fingerprint_u64(self.fingerprint, annotation.start_ns);
                self.fingerprint = fingerprint_u64(self.fingerprint, annotation.end_ns);
            }
            self.count += annotations.len() as u64;
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct ResourceMetrics {
        user_seconds: f64,
        system_seconds: f64,
        peak_rss_bytes: u64,
    }

    impl ResourceMetrics {
        fn report(self, elapsed: Duration) -> String {
            let cpu_cores = (self.user_seconds + self.system_seconds) / elapsed.as_secs_f64();
            format!(
                "cpu_user_s={:.3} cpu_system_s={:.3} avg_cpu_cores={:.2} peak_rss_mib={:.1}",
                self.user_seconds,
                self.system_seconds,
                cpu_cores,
                self.peak_rss_bytes as f64 / (1024.0 * 1024.0),
            )
        }
    }

    #[cfg(unix)]
    fn resource_usage() -> Option<ResourceMetrics> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `usage` points to writable storage for exactly one
        // `libc::rusage`, and `getrusage` initializes it on success.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: the successful call above initialized the structure.
        let usage = unsafe { usage.assume_init() };
        let timeval_seconds =
            |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1_000_000.0;
        #[cfg(target_os = "macos")]
        let peak_rss_bytes = usage.ru_maxrss.max(0) as u64;
        #[cfg(not(target_os = "macos"))]
        let peak_rss_bytes = (usage.ru_maxrss.max(0) as u64).saturating_mul(1024);
        Some(ResourceMetrics {
            user_seconds: timeval_seconds(usage.ru_utime),
            system_seconds: timeval_seconds(usage.ru_stime),
            peak_rss_bytes,
        })
    }

    #[cfg(not(unix))]
    fn resource_usage() -> Option<ResourceMetrics> {
        None
    }

    fn resource_delta(
        before: Option<ResourceMetrics>,
        after: Option<ResourceMetrics>,
    ) -> Option<ResourceMetrics> {
        let (before, after) = (before?, after?);
        Some(ResourceMetrics {
            user_seconds: (after.user_seconds - before.user_seconds).max(0.0),
            system_seconds: (after.system_seconds - before.system_seconds).max(0.0),
            peak_rss_bytes: after.peak_rss_bytes,
        })
    }

    #[derive(Debug, Clone, Copy, Default)]
    struct LatencyMetrics {
        median_us: f64,
        p95_us: f64,
    }

    impl LatencyMetrics {
        fn from_durations(mut durations: Vec<Duration>) -> Self {
            if durations.is_empty() {
                return Self::default();
            }
            durations.sort_unstable();
            let median = durations[durations.len() / 2];
            let p95 = durations[(durations.len() * 95 / 100).min(durations.len() - 1)];
            Self {
                median_us: median.as_secs_f64() * 1_000_000.0,
                p95_us: p95.as_secs_f64() * 1_000_000.0,
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct IndexedStoreMetrics {
        data_bytes: u64,
        bytes_per_word: f64,
        blocks: usize,
        restarts: u64,
        validation: Duration,
        exact_cold: LatencyMetrics,
        exact_warm: LatencyMetrics,
        presence: LatencyMetrics,
        cursor: LatencyMetrics,
        decoded_cache: DecodedBlockCacheStats,
    }

    struct BenchResult {
        mode: BenchMode,
        sink: SinkKind,
        setup: Duration,
        elapsed: Duration,
        samples: u64,
        samplerate_hz: f64,
        words: u64,
        words_measured: bool,
        fingerprint: Option<u64>,
        fingerprint_scope: Option<&'static str>,
        resources: Option<ResourceMetrics>,
        selected_protocol: &'static str,
        strobe_activity_ratio: Option<f64>,
        workers: usize,
        max_outstanding: usize,
        max_reorder: usize,
        estimated_fragment_bytes: usize,
        viewer_drain_ns: u64,
        viewer_append_ns: u64,
        viewer_batches: u64,
        indexed_store: Option<IndexedStoreMetrics>,
    }

    impl BenchResult {
        fn print(&self) {
            let seconds = self.elapsed.as_secs_f64();
            let capture_seconds = self.samples as f64 / self.samplerate_hz;
            let msamples_per_second = self.samples as f64 / seconds / 1_000_000.0;
            let realtime = capture_seconds / seconds;
            let resources = self
                .resources
                .map(|resources| resources.report(self.elapsed))
                .unwrap_or_else(|| "cpu_user_s=unavailable cpu_system_s=unavailable avg_cpu_cores=unavailable peak_rss_mib=unavailable".to_string());
            let activity = self
                .strobe_activity_ratio
                .map(|ratio| format!("{ratio:.6}"))
                .unwrap_or_else(|| "unavailable".to_string());
            if matches!(self.sink, SinkKind::Viewer) {
                let output_hash = self
                    .fingerprint
                    .map(|fingerprint| format!("{fingerprint:016x}"))
                    .unwrap_or_else(|| "unmeasured".to_string());
                println!(
                    "mode={:?} protocol={} workers={} queue_peak={} reorder_peak={} fragment_mib={:.1} strobe_activity_ratio={} sink={:?} samples={} words_indexed={} output_hash={} hash_scope={} viewer_drain_s={:.3} viewer_append_s={:.3} viewer_batches={} setup_s={:.3} run_s={:.3} capture_s={:.3} MSamples_s={:.3} realtime_x={:.3} {}",
                    self.mode,
                    self.selected_protocol,
                    self.workers,
                    self.max_outstanding,
                    self.max_reorder,
                    self.estimated_fragment_bytes as f64 / (1024.0 * 1024.0),
                    activity,
                    self.sink,
                    self.samples,
                    self.words,
                    output_hash,
                    self.fingerprint_scope.unwrap_or("unmeasured"),
                    self.viewer_drain_ns as f64 / 1_000_000_000.0,
                    self.viewer_append_ns as f64 / 1_000_000_000.0,
                    self.viewer_batches,
                    self.setup.as_secs_f64(),
                    seconds,
                    capture_seconds,
                    msamples_per_second,
                    realtime,
                    resources,
                );
                if let Some(store) = self.indexed_store {
                    let requests = store.decoded_cache.hits + store.decoded_cache.misses;
                    let hit_rate = if requests == 0 {
                        0.0
                    } else {
                        store.decoded_cache.hits as f64 / requests as f64
                    };
                    println!(
                        "indexed_store data_bytes={} bytes_per_word={:.3} blocks={} restarts={} validation_s={:.3} exact_cold_median_us={:.1} exact_cold_p95_us={:.1} exact_warm_median_us={:.1} exact_warm_p95_us={:.1} presence_median_us={:.1} presence_p95_us={:.1} cursor_median_us={:.1} cursor_p95_us={:.1} decoded_cache_hits={} decoded_cache_misses={} decoded_cache_hit_rate={:.3} decoded_cache_mib={:.1}",
                        store.data_bytes,
                        store.bytes_per_word,
                        store.blocks,
                        store.restarts,
                        store.validation.as_secs_f64(),
                        store.exact_cold.median_us,
                        store.exact_cold.p95_us,
                        store.exact_warm.median_us,
                        store.exact_warm.p95_us,
                        store.presence.median_us,
                        store.presence.p95_us,
                        store.cursor.median_us,
                        store.cursor.p95_us,
                        store.decoded_cache.hits,
                        store.decoded_cache.misses,
                        hit_rate,
                        store.decoded_cache.memory_bytes as f64 / (1024.0 * 1024.0),
                    );
                }
            } else if self.words_measured {
                let mwords_per_second = self.words as f64 / seconds / 1_000_000.0;
                println!(
                    "mode={:?} protocol={} workers={} queue_peak={} reorder_peak={} fragment_mib={:.1} strobe_activity_ratio={} sink={:?} samples={} words={} output_hash={:016x} hash_scope={} setup_s={:.3} run_s={:.3} capture_s={:.3} MSamples_s={:.3} MWords_s={:.3} realtime_x={:.3} {}",
                    self.mode,
                    self.selected_protocol,
                    self.workers,
                    self.max_outstanding,
                    self.max_reorder,
                    self.estimated_fragment_bytes as f64 / (1024.0 * 1024.0),
                    activity,
                    self.sink,
                    self.samples,
                    self.words,
                    self.fingerprint.expect("count result has a fingerprint"),
                    self.fingerprint_scope
                        .expect("count result has a hash scope"),
                    self.setup.as_secs_f64(),
                    seconds,
                    capture_seconds,
                    msamples_per_second,
                    mwords_per_second,
                    realtime,
                    resources,
                );
            } else {
                println!(
                    "mode={:?} protocol={} workers={} queue_peak={} reorder_peak={} fragment_mib={:.1} strobe_activity_ratio={} sink={:?} samples={} words=unmeasured output_hash=unmeasured setup_s={:.3} run_s={:.3} capture_s={:.3} MSamples_s={:.3} realtime_x={:.3} {}",
                    self.mode,
                    self.selected_protocol,
                    self.workers,
                    self.max_outstanding,
                    self.max_reorder,
                    self.estimated_fragment_bytes as f64 / (1024.0 * 1024.0),
                    activity,
                    self.sink,
                    self.samples,
                    self.setup.as_secs_f64(),
                    seconds,
                    capture_seconds,
                    msamples_per_second,
                    realtime,
                    resources,
                );
            }
        }
    }

    fn benchmark_indexed_store(
        lane: &signal_processing::IndexedAnnotationLane,
        query_samples: usize,
    ) -> Result<(OutputStats, IndexedStoreMetrics), Box<dyn std::error::Error>> {
        let storage = lane.storage_metadata();
        let metadata = lane.metadata();
        let first = metadata.first_timestamp_ns.unwrap_or(0);
        let end = metadata.extent_end_ns.unwrap_or(first).max(first);
        let span = end.saturating_sub(first);
        let sample_count = query_samples.max(1);
        let positions: Vec<_> = (0..sample_count)
            .map(|index| {
                let permuted = index.wrapping_mul(97) % sample_count;
                first.saturating_add(
                    (u128::from(span) * permuted as u128 / sample_count as u128) as u64,
                )
            })
            .collect();

        reset_decoded_block_cache_stats();
        let mut exact_cold = Vec::with_capacity(sample_count);
        let mut exact_warm = Vec::with_capacity(sample_count);
        for &position in &positions {
            let query_end = position.saturating_add(100_000).min(end);
            let started = Instant::now();
            let cold = lane.query().exact_window(position, query_end, 4_096)?;
            exact_cold.push(started.elapsed());
            if !cold.complete {
                return Err("exact validation window exceeded 4,096 words".into());
            }
            let started = Instant::now();
            let warm = lane.query().exact_window(position, query_end, 4_096)?;
            exact_warm.push(started.elapsed());
            if cold.annotations != warm.annotations {
                return Err("cold and warm exact queries returned different words".into());
            }
        }

        let mut presence = Vec::with_capacity(sample_count);
        for _ in 0..sample_count {
            let started = Instant::now();
            let buckets = lane.query().presence_window(first, end, 1_920)?;
            presence.push(started.elapsed());
            if metadata.total_word_count > 0 && buckets.is_empty() {
                return Err("full-extent presence query returned no buckets".into());
            }
        }

        let mut cursor = Vec::with_capacity(sample_count);
        for &position in &positions {
            let started = Instant::now();
            let _ = lane.query().nearest_boundary(position, 1_000_000)?;
            cursor.push(started.elapsed());
        }
        let decoded_cache = decoded_block_cache_stats();

        let validation_started = Instant::now();
        let mut stats = OutputStats::default();
        let mut blocks = 0usize;
        let mut restarts = 0u64;
        lane.store().visit_committed_blocks(|block| {
            blocks += 1;
            restarts += u64::from(block.restart_count);
            stats.extend_words(block.words);
        })?;
        let validation = validation_started.elapsed();
        if stats.count != metadata.total_word_count {
            return Err(format!(
                "indexed store contains {} words but metadata reports {}",
                stats.count, metadata.total_word_count
            )
            .into());
        }

        Ok((
            stats,
            IndexedStoreMetrics {
                data_bytes: storage.committed_data_len,
                bytes_per_word: if stats.count == 0 {
                    0.0
                } else {
                    storage.committed_data_len as f64 / stats.count as f64
                },
                blocks,
                restarts,
                validation,
                exact_cold: LatencyMetrics::from_durations(exact_cold),
                exact_warm: LatencyMetrics::from_durations(exact_warm),
                presence: LatencyMetrics::from_durations(presence),
                cursor: LatencyMetrics::from_durations(cursor),
                decoded_cache,
            },
        ))
    }

    fn benchmark_cache_key(mode: BenchMode, samples: u64) -> [u8; 32] {
        let mut key = [0u8; 32];
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_le_bytes();
        key[..16].copy_from_slice(&now);
        key[16..24].copy_from_slice(&samples.to_le_bytes());
        key[24..28].copy_from_slice(&std::process::id().to_le_bytes());
        key[28] = match mode {
            BenchMode::Indexed => 1,
            BenchMode::Stream => 2,
            BenchMode::Auto => 3,
            BenchMode::Both => 4,
        };
        key
    }

    fn run(args: &Args, mode: BenchMode) -> Result<BenchResult, Box<dyn std::error::Error>> {
        let decoded_cache_bytes = args
            .decoded_cache_mib
            .checked_mul(1024 * 1024)
            .ok_or("--decoded-cache-mib is too large")?;
        configure_decoded_block_cache(decoded_cache_bytes);
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
        let input_strategy = match mode {
            BenchMode::Indexed => ParallelInputStrategy::Indexed,
            BenchMode::Stream => ParallelInputStrategy::PackedStream,
            BenchMode::Auto => ParallelInputStrategy::Auto,
            BenchMode::Both => unreachable!("Both is expanded by main"),
        };
        let decoder = ParallelDecoder::new(args.data.len(), args.trigger.into(), cs_polarity)
            .with_input_strategy(input_strategy)
            .with_parallel_workers(args.workers);
        let parallel_workers = decoder.parallel_workers();
        let parallel_metrics = decoder.parallel_metrics();

        let setup_start = Instant::now();
        let strobe_activity_ratio = if matches!(mode, BenchMode::Auto) {
            source
                .edge_query(args.strobe, &[])
                .and_then(|query| query.activity_ratio_hint())
        } else {
            None
        };
        let selected_protocol = match mode {
            BenchMode::Indexed => BenchMode::Indexed.protocol_name(),
            BenchMode::Stream => BenchMode::Stream.protocol_name(),
            BenchMode::Auto => strobe_activity_ratio
                .map(ParallelDecoder::auto_protocol_for_activity_ratio)
                .map(|protocol| match protocol {
                    ProtocolKind::Stream => "packed-stream",
                    ProtocolKind::EdgeQuery => "edge-query",
                })
                .unwrap_or("auto-fallback"),
            BenchMode::Both => unreachable!("Both is expanded by main"),
        };
        let workers = if selected_protocol == "packed-stream" {
            parallel_workers
        } else {
            1
        };

        let output_stats = Arc::new(Mutex::new(OutputStats::default()));
        let retained_words = Arc::new(Mutex::new(Vec::<Word>::new()));
        let viewer_metrics = ViewerSinkMetrics::default();
        let mut viewer_store = None;
        let word_store_directory = tempfile::tempdir()?;
        let sink_port;
        let mut pipeline = Pipeline::new().with_default_buffer_size(args.buffer);
        pipeline.add_process("source", source)?;
        pipeline.add_process("decoder", decoder)?;
        match args.sink {
            SinkKind::Discard => {
                sink_port = None;
            }
            SinkKind::Count => {
                pipeline.add_process("sink", CountWords::new(Arc::clone(&output_stats)))?;
                sink_port = Some("words");
            }
            SinkKind::Retain => {
                pipeline.add_process("sink", RetainWords::new(Arc::clone(&retained_words)))?;
                sink_port = Some("words");
            }
            SinkKind::Viewer => {
                let store = DerivedLanes::new();
                let mut store_config = LiveStoreConfig {
                    directory: word_store_directory.path().to_path_buf(),
                    ..LiveStoreConfig::default()
                };
                store_config.block.max_words = args.store_block_words.max(1);
                store_config.block.restart_interval = args.store_restart_interval.max(1);
                if matches!(args.cache, CacheKind::Persistent) {
                    store_config.persistence = Some(PersistentStoreConfig::new(
                        word_store_directory.path(),
                        benchmark_cache_key(mode, samples),
                    ));
                }
                pipeline.add_process(
                    "sink",
                    ViewerSink::new(store.clone())
                        .with_retention(ViewerRetention::MaxEntries(args.viewer_max_entries.max(1)))
                        .with_metrics(viewer_metrics.clone())
                        .with_word_store_config(store_config)
                        .with_lane(ViewerLaneKind::Words, "parallel"),
                )?;
                viewer_store = Some(store);
                sink_port = Some("in0");
            }
            SinkKind::File => {
                let output = word_store_directory.path().join("decoded.bin");
                pipeline.add_process(
                    "sink",
                    BinaryFileWriter::new().with_filename(output.display().to_string()),
                )?;
                sink_port = Some("data");
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
        if let Some(sink_port) = sink_port {
            pipeline.connect("decoder", "words", "sink", sink_port)?;
        }

        let scheduler = pipeline.build()?;
        let setup = setup_start.elapsed();
        let resources_before = resource_usage();
        let run_start = Instant::now();
        scheduler.wait();
        let elapsed = run_start.elapsed();
        let resources = resource_delta(resources_before, resource_usage());
        let parallel_metrics = parallel_metrics.snapshot();
        let viewer_metrics = viewer_metrics.snapshot();

        let mut indexed_store = None;
        let (stats, fingerprint_scope) = if let Some(store) = viewer_store {
            let lanes = store.read();
            match lanes.first().map(|lane| &lane.data) {
                Some(DerivedLaneData::Annotations(annotations)) => {
                    let mut stats = OutputStats::default();
                    stats.extend_annotations(annotations);
                    (stats, Some("retained-annotations"))
                }
                Some(DerivedLaneData::IndexedAnnotations(indexed)) => {
                    let (stats, metrics) = benchmark_indexed_store(indexed, args.query_samples)?;
                    indexed_store = Some(metrics);
                    (stats, Some("indexed-store"))
                }
                _ => (OutputStats::default(), Some("retained-annotations")),
            }
        } else if matches!(args.sink, SinkKind::Count) {
            (*output_stats.lock().unwrap(), Some("decoded-words"))
        } else if matches!(args.sink, SinkKind::Retain) {
            let mut stats = OutputStats::default();
            stats.extend_words(&retained_words.lock().unwrap());
            (stats, Some("retained-words"))
        } else {
            (OutputStats::default(), None)
        };

        Ok(BenchResult {
            mode,
            sink: args.sink,
            setup,
            elapsed,
            samples,
            samplerate_hz,
            words: stats.count,
            words_measured: matches!(
                args.sink,
                SinkKind::Count | SinkKind::Retain | SinkKind::Viewer
            ),
            fingerprint: fingerprint_scope.map(|_| stats.fingerprint),
            fingerprint_scope,
            resources,
            selected_protocol,
            strobe_activity_ratio,
            workers,
            max_outstanding: parallel_metrics.max_outstanding,
            max_reorder: parallel_metrics.max_reorder,
            estimated_fragment_bytes: parallel_metrics.estimated_fragment_bytes,
            viewer_drain_ns: viewer_metrics.drain_ns,
            viewer_append_ns: viewer_metrics.append_ns,
            viewer_batches: viewer_metrics.batches,
            indexed_store,
        })
    }

    pub(crate) fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        if !(1..=8).contains(&args.workers) {
            return Err("--workers must be between 1 and 8".into());
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
            BenchMode::Auto => &[BenchMode::Auto],
            BenchMode::Both => &[BenchMode::Indexed, BenchMode::Stream, BenchMode::Auto],
        };
        let mut count_reference: Option<(BenchMode, u64, u64)> = None;
        for &mode in modes {
            let result = run(&args, mode)?;
            result.print();
            if matches!(args.sink, SinkKind::Count | SinkKind::Retain) {
                let fingerprint = result
                    .fingerprint
                    .expect("count result must include a fingerprint");
                if let Some((reference_mode, reference_count, reference_fingerprint)) =
                    count_reference
                {
                    if result.words != reference_count || fingerprint != reference_fingerprint {
                        return Err(format!(
                            "decoded output mismatch: {:?} produced {} words/{:016x}, {:?} produced {} words/{:016x}",
                            reference_mode,
                            reference_count,
                            reference_fingerprint,
                            result.mode,
                            result.words,
                            fingerprint,
                        )
                        .into());
                    }
                    println!(
                        "verification=match modes={:?},{:?} words={} output_hash={:016x}",
                        reference_mode, result.mode, result.words, fingerprint
                    );
                } else {
                    count_reference = Some((result.mode, result.words, fingerprint));
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use std::fs::File;
        use std::io::Write;

        use zip::write::SimpleFileOptions;

        use super::*;

        fn set_packed_bit(data: &mut [u8], sample: usize, value: bool) {
            if value {
                data[sample / 8] |= 1 << (sample % 8);
            }
        }

        fn write_sparse_capture(path: &std::path::Path) {
            const SAMPLES: usize = 65_536;
            const TRIGGERS: [usize; 3] = [1_000, 30_000, 60_000];
            let file = File::create(path).unwrap();
            let mut archive = zip::ZipWriter::new(file);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            archive.start_file("header", options).unwrap();
            archive
                .write_all(
                    b"total probes = 3\nsamplerate = 1 MHz\ntotal samples = 65536\ntotal blocks = 1\nprobe0 = D0\nprobe1 = D1\nprobe2 = Clock\n",
                )
                .unwrap();

            let mut channels = vec![vec![0u8; SAMPLES / 8]; 3];
            for &trigger in &TRIGGERS {
                set_packed_bit(&mut channels[2], trigger, true);
            }
            for sample in 0..SAMPLES {
                let value = match sample {
                    0..1_000 => 0,
                    1_000..30_000 => 1,
                    30_000..60_000 => 2,
                    _ => 3,
                };
                set_packed_bit(&mut channels[0], sample, value & 1 != 0);
                set_packed_bit(&mut channels[1], sample, value & 2 != 0);
            }
            for (channel, data) in channels.iter().enumerate() {
                archive
                    .start_file(format!("L-{channel}/0"), options)
                    .unwrap();
                archive.write_all(data).unwrap();
            }
            archive.finish().unwrap();
        }

        fn write_dense_capture(path: &std::path::Path) {
            const SAMPLES: usize = 65_536;
            let file = File::create(path).unwrap();
            let mut archive = zip::ZipWriter::new(file);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            archive.start_file("header", options).unwrap();
            archive
                .write_all(
                    b"total probes = 3\nsamplerate = 1 MHz\ntotal samples = 65536\ntotal blocks = 1\nprobe0 = D0\nprobe1 = D1\nprobe2 = Clock\n",
                )
                .unwrap();

            let mut channels = vec![vec![0u8; SAMPLES / 8]; 3];
            for sample in 0..SAMPLES {
                let value = (sample / 4) & 0b11;
                set_packed_bit(&mut channels[0], sample, value & 1 != 0);
                set_packed_bit(&mut channels[1], sample, value & 2 != 0);
                set_packed_bit(&mut channels[2], sample, sample % 4 == 1 || sample % 4 == 2);
            }
            for (channel, data) in channels.iter().enumerate() {
                archive
                    .start_file(format!("L-{channel}/0"), options)
                    .unwrap();
                archive.write_all(data).unwrap();
            }
            archive.finish().unwrap();
        }

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
            assert_eq!(args.viewer_max_entries, 4_000_000);
            assert_eq!(args.workers, 4);
        }

        #[test]
        fn capture_path_is_required() {
            assert!(Args::try_parse_from(["parallel-decoder-bench"]).is_err());
        }

        #[test]
        fn parses_transport_free_discard_sink() {
            let args = Args::try_parse_from([
                "parallel-decoder-bench",
                "capture.dsl",
                "--sink",
                "discard",
            ])
            .unwrap();

            assert!(matches!(args.sink, SinkKind::Discard));
        }

        #[test]
        fn word_fingerprint_is_stable_and_order_sensitive() {
            let first = Word::spanning(0x12, 1_000, 50);
            let second = Word::spanning(0x27, 2_000, 75);
            let mut a = OutputStats::default();
            a.extend_words(&[first, second]);
            let mut b = OutputStats::default();
            b.extend_words(&[first]);
            b.extend_words(&[second]);
            let mut reversed = OutputStats::default();
            reversed.extend_words(&[second, first]);

            assert_eq!(a.count, 2);
            assert_eq!(a.fingerprint, b.fingerprint);
            assert_ne!(a.fingerprint, reversed.fingerprint);
        }

        #[test]
        fn sparse_capture_matches_between_indexed_and_packed_protocols() {
            let directory = tempfile::tempdir().unwrap();
            let capture = directory.path().join("sparse.dsl");
            write_sparse_capture(&capture);
            let source = DslFileSource::new(&capture, 3).unwrap();
            let activity = source
                .edge_query(2, &[])
                .and_then(|query| query.activity_ratio_hint())
                .expect("file-backed strobe should expose an activity hint");
            assert!(activity < 0.01, "sparse activity ratio was {activity}");
            let args = Args::try_parse_from([
                "parallel-decoder-bench",
                capture.to_str().unwrap(),
                "--samples",
                "65536",
                "--mode",
                "both",
                "--sink",
                "count",
                "--strobe",
                "2",
                "--data",
                "0,1",
                "--trigger",
                "rising",
                "--workers",
                "4",
            ])
            .unwrap();

            let indexed = run(&args, BenchMode::Indexed).unwrap();
            let packed = run(&args, BenchMode::Stream).unwrap();
            let auto = run(&args, BenchMode::Auto).unwrap();
            assert_eq!(indexed.words, 3);
            assert_eq!(indexed.words, packed.words);
            assert_eq!(indexed.fingerprint, packed.fingerprint);
            assert_eq!(indexed.fingerprint, auto.fingerprint);
            assert_eq!(auto.selected_protocol, "edge-query");
            assert_eq!(auto.max_outstanding, 0);
            assert!(packed.max_outstanding > 0);
        }

        #[test]
        fn dense_capture_auto_negotiates_parallel_packed_streaming() {
            let directory = tempfile::tempdir().unwrap();
            let capture = directory.path().join("dense.dsl");
            write_dense_capture(&capture);
            let source = DslFileSource::new(&capture, 3).unwrap();
            let activity = source
                .edge_query(2, &[])
                .and_then(|query| query.activity_ratio_hint())
                .expect("file-backed strobe should expose an activity hint");
            assert!(activity > 0.99, "dense activity ratio was {activity}");
            let args = Args::try_parse_from([
                "parallel-decoder-bench",
                capture.to_str().unwrap(),
                "--samples",
                "65536",
                "--mode",
                "both",
                "--sink",
                "count",
                "--strobe",
                "2",
                "--data",
                "0,1",
                "--trigger",
                "rising",
                "--workers",
                "4",
            ])
            .unwrap();

            let indexed = run(&args, BenchMode::Indexed).unwrap();
            let packed = run(&args, BenchMode::Stream).unwrap();
            let auto = run(&args, BenchMode::Auto).unwrap();
            assert_eq!(indexed.words, 16_384);
            assert_eq!(indexed.fingerprint, packed.fingerprint);
            assert_eq!(indexed.fingerprint, auto.fingerprint);
            assert_eq!(auto.selected_protocol, "packed-stream");
            assert!(auto.max_outstanding > 0);
        }
    }
        }
    }
}
