//! Parallel bus decoder for block-based processing.
//!
//! Accepts SampleBlock inputs for high-bandwidth signals (strobe, data, CS)
//! and Sample inputs for low-bandwidth control signals (enable_signal).
//! Outputs Word events.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracing::debug;

use signal_processing::capture::CaptureTransition;
use signal_processing::edge_query::EdgeQuery;
use signal_processing::errors::{WorkError, WorkResult};
use signal_processing::events::Word;
use signal_processing::node::{InputProtocolCandidate, ProcessNode};
use signal_processing::ports::{InputPort, OutputPort};
use signal_processing::protocol::ProtocolKind;
use signal_processing::receiver::Receiver;
use signal_processing::sample::{Sample, SampleBlock};

use super::types::{CsPolarity, Endianness, ParallelInputStrategy, StrobeMode};

#[cfg_attr(target_arch = "wasm32", path = "parallel_decoder/sequential_worker.rs")]
#[cfg_attr(
    not(target_arch = "wasm32"),
    path = "parallel_decoder/parallel_worker.rs"
)]
mod worker_backend;

use self::worker_backend::ParallelStreamState;

#[derive(Clone)]
pub struct ParallelDecoderMetrics {
    inner: Arc<ParallelDecoderMetricsInner>,
}

struct ParallelDecoderMetricsInner {
    workers: AtomicUsize,
    max_outstanding: AtomicUsize,
    max_reorder: AtomicUsize,
    max_fragment_bytes: AtomicUsize,
}

impl Default for ParallelDecoderMetrics {
    fn default() -> Self {
        Self {
            inner: Arc::new(ParallelDecoderMetricsInner {
                workers: AtomicUsize::new(1),
                max_outstanding: AtomicUsize::new(0),
                max_reorder: AtomicUsize::new(0),
                max_fragment_bytes: AtomicUsize::new(0),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParallelDecoderMetricsSnapshot {
    pub workers: usize,
    pub max_outstanding: usize,
    pub max_reorder: usize,
    pub estimated_fragment_bytes: usize,
}

impl ParallelDecoderMetrics {
    pub fn snapshot(&self) -> ParallelDecoderMetricsSnapshot {
        let max_outstanding = self.inner.max_outstanding.load(Ordering::Relaxed);
        let max_fragment_bytes = self.inner.max_fragment_bytes.load(Ordering::Relaxed);
        ParallelDecoderMetricsSnapshot {
            workers: self.inner.workers.load(Ordering::Relaxed),
            max_outstanding,
            max_reorder: self.inner.max_reorder.load(Ordering::Relaxed),
            estimated_fragment_bytes: max_outstanding.saturating_mul(max_fragment_bytes),
        }
    }
}

/// Parallel bus decoder node (block-based)
///
/// Inputs:
///   - Block inputs: strobe_block, d0_block..dN_block, cs_block — all SampleBlock
///   - Edge input: enable_signal — Sample (from SPI controller)
///
/// Output: Word events
pub struct ParallelDecoder {
    name: String,
    num_data_bits: usize,
    mode: StrobeMode,
    cs_polarity: CsPolarity,
    input_strategy: ParallelInputStrategy,

    /// Bus cycles assembled into one output word (1 = one cycle per word)
    cycles_per_word: usize,
    /// Cycle order when `cycles_per_word > 1`
    endianness: Endianness,

    /// Putback buffer for enable_signal (edge-based Sample input)
    enable_buffer: VecDeque<Sample>,

    /// Current enable state from edge-based enable_signal
    current_enable_value: bool,
    /// Indexed-path timestamp up to which the current enable value is known.
    /// Streamed enable handling peeks directly at the next queued sample.
    next_enable_change_position: u64,

    last_strobe_value: bool,
    work_call_count: usize,
    total_words_emitted: u64,

    /// Word-assembly state (persists across blocks)
    assembly_value: u64,
    assembly_cycles: usize,
    assembly_first_ts: u64,

    /// Query-mode strobe search cursor, persisted across `work()` calls
    /// (the index-query equivalent of the streaming block reader's
    /// implicit position). Unused in streaming mode.
    query_strobe_position: u64,

    /// Reused allocations for the indexed batch path.
    query_buffers: QueryBuffers,

    /// Current aligned packed blocks and cursor for bounded streamed work.
    stream_blocks: StreamBlockState,
    /// Next fragment expected by the ordered stream merge.
    next_stream_merge_sequence: u64,
    /// Maximum number of packed scan jobs this decoder may execute at once.
    /// A value of one selects the sequential path.
    parallel_workers: usize,
    parallel_metrics: ParallelDecoderMetrics,
}

impl ParallelDecoder {
    pub const DEFAULT_PARALLEL_WORKERS: usize = 4;

    /// Above this fraction of active 64-sample strobe groups, packed scans
    /// are preferred over indexed point queries in Auto mode.
    pub const AUTO_PACKED_ACTIVITY_RATIO: f64 = 0.25;

    pub fn auto_protocol_for_activity_ratio(activity_ratio: f64) -> ProtocolKind {
        if activity_ratio >= Self::AUTO_PACKED_ACTIVITY_RATIO {
            ProtocolKind::Stream
        } else {
            ProtocolKind::EdgeQuery
        }
    }

    /// Create a new parallel decoder
    ///
    /// # Arguments
    ///
    /// * `num_data_bits` - Number of data bits (1-64)
    /// * `mode` - Strobe trigger mode
    /// * `cs_polarity` - CS polarity: ActiveLow, ActiveHigh, or Disabled
    pub fn new(num_data_bits: usize, mode: StrobeMode, cs_polarity: CsPolarity) -> Self {
        assert!(
            num_data_bits > 0 && num_data_bits <= 64,
            "Data bits must be 1-64"
        );

        Self {
            name: "parallel_decoder".to_string(),
            num_data_bits,
            mode,
            cs_polarity,
            input_strategy: ParallelInputStrategy::Auto,
            cycles_per_word: 1,
            endianness: Endianness::default(),
            enable_buffer: VecDeque::new(),
            current_enable_value: false,
            next_enable_change_position: 0,
            last_strobe_value: false,
            work_call_count: 0,
            total_words_emitted: 0,
            assembly_value: 0,
            assembly_cycles: 0,
            assembly_first_ts: 0,
            query_strobe_position: 0,
            query_buffers: QueryBuffers::default(),
            stream_blocks: StreamBlockState::default(),
            next_stream_merge_sequence: 0,
            parallel_workers: Self::DEFAULT_PARALLEL_WORKERS,
            parallel_metrics: ParallelDecoderMetrics::default(),
        }
    }

    /// With custom name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_input_strategy(mut self, strategy: ParallelInputStrategy) -> Self {
        self.input_strategy = strategy;
        self
    }

    /// Run packed fragment scans concurrently on the shared native worker
    /// pool. The wasm runtime and a value of one remain sequential.
    pub fn with_parallel_workers(mut self, workers: usize) -> Self {
        self.parallel_workers = workers.clamp(1, 8);
        self
    }

    pub fn parallel_workers(&self) -> usize {
        worker_backend::effective_workers(self.parallel_workers, &self.parallel_metrics)
    }

    pub fn parallel_metrics(&self) -> ParallelDecoderMetrics {
        self.parallel_metrics.clone()
    }

    /// Assemble `cycles` successive bus samples into one output word.
    /// `endianness` selects whether the first cycle is the least (`Little`)
    /// or most (`Big`) significant part. Incomplete words at a CS/enable
    /// boundary are dropped.
    pub fn with_word_assembly(mut self, cycles: usize, endianness: Endianness) -> Self {
        assert!(
            cycles >= 1 && cycles * self.num_data_bits <= 64,
            "cycles_per_word must be >= 1 and fit in 64 bits"
        );
        self.cycles_per_word = cycles;
        self.endianness = endianness;
        self
    }
}

impl ProcessNode for ParallelDecoder {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        // Block inputs: strobe_block + d0_block..dN_block + cs_block
        // Edge input: enable_signal
        let block_inputs = 1 + self.num_data_bits + 1; // strobe + data + cs
        let edge_inputs = 1; // enable_signal
        block_inputs + edge_inputs
    }

    fn num_outputs(&self) -> usize {
        1 // Word output
    }

    fn input_schema(&self) -> Vec<signal_processing::ports::PortSchema> {
        use signal_processing::ports::{PortDirection, PortSchema};

        // A level trigger must inspect every sample and therefore always
        // streams. Edge-triggered modes honor the explicit strategy; Auto
        // keeps the existing indexed-first preference.
        let input_protocols = if matches!(self.mode, StrobeMode::HighLevel | StrobeMode::LowLevel) {
            vec![ProtocolKind::Stream]
        } else {
            match self.input_strategy {
                ParallelInputStrategy::Auto => {
                    vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream]
                }
                ParallelInputStrategy::PackedStream => vec![ProtocolKind::Stream],
                ParallelInputStrategy::Indexed => vec![ProtocolKind::EdgeQuery],
            }
        };

        let mut schemas = Vec::new();

        // Block inputs first
        schemas.push(
            PortSchema::new::<SampleBlock>("strobe", 0, PortDirection::Input)
                .with_protocols(input_protocols.clone()),
        );

        for i in 0..self.num_data_bits {
            schemas.push(
                PortSchema::new::<SampleBlock>(format!("d{}", i), 1 + i, PortDirection::Input)
                    .with_protocols(input_protocols.clone()),
            );
        }

        schemas.push(
            PortSchema::new::<SampleBlock>("cs", 1 + self.num_data_bits, PortDirection::Input)
                .with_protocols(input_protocols.clone()),
        );

        // Edge input last
        // Enable is a low-rate level input with its own transport choice;
        // it is not constrained by how the raw packed channels arrive.
        schemas.push(
            PortSchema::new::<Sample>(
                "enable_signal",
                1 + self.num_data_bits + 1,
                PortDirection::Input,
            )
            .with_protocols(vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream]),
        );

        schemas
    }

    fn output_schema(&self) -> Vec<signal_processing::ports::PortSchema> {
        use signal_processing::ports::{PortDirection, PortSchema};

        vec![PortSchema::new::<Word>("words", 0, PortDirection::Output)]
    }

    fn select_input_protocols(
        &self,
        candidates: &[Option<InputProtocolCandidate>],
    ) -> Vec<Option<ProtocolKind>> {
        debug!(
            decoder = %self.name,
            strategy = ?self.input_strategy,
            candidate_count = candidates.len(),
            "selecting parallel decoder input protocols"
        );
        let schemas = self.input_schema();
        let mut selected: Vec<Option<ProtocolKind>> = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let candidate = candidate.as_ref()?;
                let accepted = &schemas.get(index)?.protocols;
                candidate
                    .offered
                    .iter()
                    .find(|protocol| accepted.contains(protocol))
                    .copied()
            })
            .collect();

        let edge_triggered = matches!(
            self.mode,
            StrobeMode::RisingEdge | StrobeMode::FallingEdge | StrobeMode::AnyEdge
        );
        if self.input_strategy != ParallelInputStrategy::Auto || !edge_triggered {
            return selected;
        }
        let Some(activity_ratio) = candidates
            .first()
            .and_then(Option::as_ref)
            .and_then(|candidate| candidate.edge_query.as_ref())
            .and_then(|query| query.activity_ratio_hint())
        else {
            return selected;
        };

        let preferred = Self::auto_protocol_for_activity_ratio(activity_ratio);
        let alternate = if preferred == ProtocolKind::Stream {
            ProtocolKind::EdgeQuery
        } else {
            ProtocolKind::Stream
        };
        let raw_inputs = 1 + self.num_data_bits + 1;
        let group_supports = |protocol| {
            candidates[..raw_inputs]
                .iter()
                .enumerate()
                .filter_map(|(index, candidate)| {
                    candidate.as_ref().map(|candidate| (index, candidate))
                })
                .all(|(index, candidate)| {
                    candidate.offered.contains(&protocol)
                        && schemas[index].protocols.contains(&protocol)
                })
        };
        let protocol = if group_supports(preferred) {
            Some(preferred)
        } else if group_supports(alternate) {
            Some(alternate)
        } else {
            None
        };
        debug!(
            decoder = %self.name,
            activity_ratio,
            ?preferred,
            ?protocol,
            "selected parallel decoder raw-input protocol"
        );
        for (index, candidate) in candidates[..raw_inputs].iter().enumerate() {
            if candidate.is_some() {
                selected[index] = protocol;
            }
        }
        selected
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        // HighLevel/LowLevel trigger on *every* sample at that level, not
        // on sparse transitions, so a skip-ahead search doesn't help them
        // (every level-held sample would be its own point query — no
        // faster than streaming, likely slower). Only the edge-triggered
        // modes benefit, and only if strobe/every data bit/cs (when it
        // matters) all negotiated EdgeQuery.
        let sparse_mode = matches!(
            self.mode,
            StrobeMode::RisingEdge | StrobeMode::FallingEdge | StrobeMode::AnyEdge
        );
        let strobe_query = inputs.first().and_then(|p| p.edge_query());
        let data_queries: Vec<Option<Arc<dyn EdgeQuery>>> = (0..self.num_data_bits)
            .map(|i| inputs.get(1 + i).and_then(|p| p.edge_query()))
            .collect();
        let cs_query = inputs
            .get(1 + self.num_data_bits)
            .and_then(|p| p.edge_query());
        let cs_needed = self.cs_polarity != CsPolarity::Disabled;

        let ready_for_query_mode = sparse_mode
            && strobe_query.is_some()
            && data_queries.iter().all(Option::is_some)
            && (!cs_needed || cs_query.is_some());

        if ready_for_query_mode {
            let mut buffers = std::mem::take(&mut self.query_buffers);
            let result = self.work_indexed(
                inputs,
                outputs,
                strobe_query.expect("checked above"),
                data_queries
                    .into_iter()
                    .map(|q| q.expect("checked above"))
                    .collect(),
                cs_query,
                &mut buffers,
            );
            self.query_buffers = buffers;
            return result;
        }

        self.work_streamed(inputs, outputs)
    }
}

/// Word-assembly accumulator, threaded through `process_trigger` calls.
struct AssemblyState {
    value: u64,
    cycles: usize,
    first_ts: u64,
}

#[derive(Default)]
struct QueryBuffers {
    edges: Vec<CaptureTransition>,
    positions: Vec<u64>,
    eligible_positions: Vec<u64>,
    reset_before: Vec<bool>,
    cs_values: Vec<bool>,
    enable_values: Vec<bool>,
    data_values: Vec<Vec<bool>>,
}

#[derive(Default)]
struct StreamBlockState {
    strobe: Option<SampleBlock>,
    data: Vec<SampleBlock>,
    cs: Option<SampleBlock>,
    offset: usize,
    next_sequence: u64,
    fragment_buffers: DecodeFragmentBuffers,
    parallel: ParallelStreamState,
}

#[derive(Debug, Default)]
struct DecodeFragmentBuffers {
    positions: Vec<u64>,
    values: Vec<u64>,
    reset_before: Vec<bool>,
}

#[derive(Debug)]
struct DecodeFragment {
    sequence: u64,
    timestamp_step: u64,
    boundary: Option<FragmentBoundary>,
    last_strobe_value: bool,
    buffers: DecodeFragmentBuffers,
    reset_after: bool,
}

#[derive(Debug, Clone, Copy)]
struct FragmentBoundary {
    position: u64,
    strobe_value: bool,
    data_value: u64,
    cs_eligible: bool,
    reset_before: bool,
}

#[derive(Debug, Clone, Copy)]
struct StreamScanConfig {
    mode: StrobeMode,
    cs_polarity: CsPolarity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnabledRange {
    start: usize,
    end: usize,
    reset_before: bool,
}

#[inline]
fn packed_word(data: &[u8], word_index: usize) -> u64 {
    let byte_index = word_index * size_of::<u64>();
    let available = data.len().saturating_sub(byte_index).min(size_of::<u64>());
    let mut bytes = [0u8; size_of::<u64>()];
    bytes[..available].copy_from_slice(&data[byte_index..byte_index + available]);
    u64::from_le_bytes(bytes)
}

#[inline]
fn packed_bit(data: &[u8], local_position: usize) -> bool {
    (data[local_position / u8::BITS as usize] >> (local_position % u8::BITS as usize)) & 1 != 0
}

#[inline]
fn bit_range_mask(start: usize, end: usize) -> u64 {
    debug_assert!(start < end && end <= u64::BITS as usize);
    let below_end = if end == u64::BITS as usize {
        u64::MAX
    } else {
        (1u64 << end) - 1
    };
    let below_start = (1u64 << start) - 1;
    below_end & !below_start
}

#[inline]
fn packed_bus_value(data: &[SampleBlock], local_index: usize) -> u64 {
    data.iter().enumerate().fold(0u64, |value, (bit, block)| {
        value | (u64::from(packed_bit(&block.data, local_index)) << bit)
    })
}

#[inline]
fn cs_eligible(config: StreamScanConfig, cs: Option<&SampleBlock>, local_index: usize) -> bool {
    match (config.cs_polarity, cs) {
        (CsPolarity::ActiveLow, Some(cs)) => !packed_bit(&cs.data, local_index),
        (CsPolarity::ActiveHigh, Some(cs)) => packed_bit(&cs.data, local_index),
        _ => true,
    }
}

fn enabled_ranges_for_window(
    enable_query: Option<&Arc<dyn EdgeQuery>>,
    enable_input: &mut Option<Receiver<'_, Sample>>,
    current_enable_value: &mut bool,
    block_start_position: u64,
    window_start: usize,
    window_end: usize,
    timestamp_step: u64,
) -> WorkResult<Vec<EnabledRange>> {
    let absolute_start = block_start_position + window_start as u64;
    let absolute_end = block_start_position + window_end as u64;

    if let Some(query) = enable_query {
        let query_err = |error: signal_processing::Error| WorkError::NodeError(error.to_string());
        let mut current = query.value_at(absolute_start).map_err(query_err)?;
        let previous = if absolute_start == 0 {
            current
        } else {
            query.value_at(absolute_start - 1).map_err(query_err)?
        };
        let mut reset_pending = !current || previous != current;
        let mut cursor = absolute_start;
        let mut ranges = Vec::new();

        while cursor < absolute_end {
            let transition = query
                .next_edge(cursor, absolute_end)
                .map_err(query_err)?
                .filter(|transition| transition.sample < absolute_end);
            let range_end = transition
                .as_ref()
                .map_or(absolute_end, |transition| transition.sample);
            if current && cursor < range_end {
                ranges.push(EnabledRange {
                    start: window_start + (cursor - absolute_start) as usize,
                    end: window_start + (range_end - absolute_start) as usize,
                    reset_before: reset_pending,
                });
                reset_pending = false;
            }
            let Some(transition) = transition else {
                break;
            };
            current = transition.value;
            if !current {
                reset_pending = true;
            }
            cursor = transition.sample;
        }
        *current_enable_value = current;
        return Ok(ranges);
    }

    let Some(enable) = enable_input else {
        *current_enable_value = true;
        return Ok(vec![EnabledRange {
            start: window_start,
            end: window_end,
            reset_before: false,
        }]);
    };

    let timestamp_step = timestamp_step.max(1);
    let end_time_ns = absolute_end.saturating_mul(timestamp_step);
    let mut cursor = absolute_start;
    let mut reset_pending = !*current_enable_value;
    let mut ranges = Vec::new();

    loop {
        match enable.peek() {
            Ok(next_edge) if next_edge.start_time_ns < end_time_ns => {
                let transition_position = next_edge
                    .start_time_ns
                    .div_ceil(timestamp_step)
                    .clamp(absolute_start, absolute_end);
                if *current_enable_value && cursor < transition_position {
                    ranges.push(EnabledRange {
                        start: window_start + (cursor - absolute_start) as usize,
                        end: window_start + (transition_position - absolute_start) as usize,
                        reset_before: reset_pending,
                    });
                    reset_pending = false;
                }
                *current_enable_value = enable.recv()?.value;
                if !*current_enable_value {
                    reset_pending = true;
                }
                cursor = cursor.max(transition_position);
            }
            Ok(_) => {
                if *current_enable_value && cursor < absolute_end {
                    ranges.push(EnabledRange {
                        start: window_start + (cursor - absolute_start) as usize,
                        end: window_end,
                        reset_before: reset_pending,
                    });
                }
                break;
            }
            Err(WorkError::Shutdown) => {
                if *current_enable_value && cursor < absolute_end {
                    ranges.push(EnabledRange {
                        start: window_start + (cursor - absolute_start) as usize,
                        end: window_end,
                        reset_before: reset_pending,
                    });
                }
                break;
            }
            Err(error) => return Err(error),
        }
    }

    Ok(ranges)
}

#[allow(clippy::too_many_arguments)]
fn scan_stream_fragment(
    config: StreamScanConfig,
    sequence: u64,
    strobe: &SampleBlock,
    data: &[SampleBlock],
    cs: Option<&SampleBlock>,
    window_start: usize,
    window_end: usize,
    enabled_ranges: &[EnabledRange],
    mut buffers: DecodeFragmentBuffers,
) -> DecodeFragment {
    buffers.positions.clear();
    buffers.values.clear();
    buffers.reset_before.clear();
    let boundary = enabled_ranges
        .first()
        .filter(|range| range.start == window_start)
        .filter(|_| {
            matches!(
                config.mode,
                StrobeMode::RisingEdge | StrobeMode::FallingEdge | StrobeMode::AnyEdge
            )
        })
        .map(|range| FragmentBoundary {
            position: strobe.start_position + window_start as u64,
            strobe_value: packed_bit(&strobe.data, window_start),
            data_value: packed_bus_value(data, window_start),
            cs_eligible: cs_eligible(config, cs, window_start),
            reset_before: range.reset_before,
        });
    let mut reset_before_next = boundary.is_none()
        && enabled_ranges
            .first()
            .is_none_or(|range| range.reset_before);
    for (range_index, range) in enabled_ranges.iter().enumerate() {
        debug_assert!(window_start <= range.start);
        debug_assert!(range.start < range.end);
        debug_assert!(range.end <= window_end);
        if range_index > 0 || (range.reset_before && boundary.is_none()) {
            reset_before_next = true;
        }
        let first_word = range.start / u64::BITS as usize;
        let last_word = (range.end - 1) / u64::BITS as usize;
        for word_index in first_word..=last_word {
            let word_start = word_index * u64::BITS as usize;
            let word = packed_word(&strobe.data, word_index);
            let previous_bit = if word_start == 0 {
                false
            } else {
                packed_bit(&strobe.data, word_start - 1)
            };
            let toggles = word ^ ((word << 1) | u64::from(previous_bit));
            let mut triggers = match config.mode {
                StrobeMode::RisingEdge => toggles & word,
                StrobeMode::FallingEdge => toggles & !word,
                StrobeMode::AnyEdge => toggles,
                StrobeMode::HighLevel => word,
                StrobeMode::LowLevel => !word,
            };
            let range_start = range.start.saturating_sub(word_start);
            let range_end = range.end.saturating_sub(word_start).min(u64::BITS as usize);
            triggers &= bit_range_mask(range_start, range_end);
            if boundary.is_some() && word_start <= window_start {
                triggers &= !(1u64 << (window_start - word_start));
            }

            while triggers != 0 {
                let bit_in_word = triggers.trailing_zeros() as usize;
                triggers &= triggers - 1;
                let local_index = word_start + bit_in_word;
                let position = strobe.start_position + local_index as u64;
                if !cs_eligible(config, cs, local_index) {
                    reset_before_next = true;
                    continue;
                }

                buffers.positions.push(position);
                buffers.values.push(packed_bus_value(data, local_index));
                buffers.reset_before.push(reset_before_next);
                reset_before_next = false;
            }
        }
    }

    DecodeFragment {
        sequence,
        timestamp_step: strobe.timestamp_step,
        boundary,
        last_strobe_value: packed_bit(&strobe.data, window_end - 1),
        buffers,
        reset_after: reset_before_next
            || enabled_ranges
                .last()
                .is_none_or(|range| range.end < window_end),
    }
}

#[allow(clippy::too_many_arguments)]
fn merge_stream_fragment(
    fragment: &DecodeFragment,
    mode: StrobeMode,
    previous_strobe_value: &mut bool,
    num_data_bits: usize,
    cycles_per_word: usize,
    endianness: Endianness,
    assembly: &mut AssemblyState,
    word_batch: &mut Option<Vec<Word>>,
) -> WorkResult<u64> {
    let mut words_emitted = 0u64;
    let buffers = &fragment.buffers;
    debug_assert_eq!(buffers.positions.len(), buffers.values.len());
    debug_assert_eq!(buffers.positions.len(), buffers.reset_before.len());
    let mut merge_sample = |position: u64, value: u64, reset_before: bool| -> WorkResult<()> {
        if reset_before {
            assembly.cycles = 0;
            assembly.value = 0;
        }
        let timestamp_ns = position.saturating_mul(fragment.timestamp_step);

        if assembly.cycles == 0 {
            assembly.first_ts = timestamp_ns;
        }
        match endianness {
            Endianness::Little => {
                assembly.value |= value << (assembly.cycles * num_data_bits);
            }
            Endianness::Big => {
                assembly.value = (assembly.value << num_data_bits) | value;
            }
        }
        assembly.cycles += 1;
        if assembly.cycles < cycles_per_word {
            return Ok(());
        }

        if let Some(batch) = word_batch {
            batch.push(Word::spanning(
                assembly.value,
                assembly.first_ts,
                timestamp_ns.saturating_sub(assembly.first_ts),
            ));
        }
        assembly.value = 0;
        assembly.cycles = 0;
        words_emitted += 1;
        Ok(())
    };

    let mut boundary_reset = fragment
        .boundary
        .is_some_and(|boundary| boundary.reset_before);
    if let Some(boundary) = fragment.boundary {
        let triggered = match mode {
            StrobeMode::RisingEdge => !*previous_strobe_value && boundary.strobe_value,
            StrobeMode::FallingEdge => *previous_strobe_value && !boundary.strobe_value,
            StrobeMode::AnyEdge => *previous_strobe_value != boundary.strobe_value,
            StrobeMode::HighLevel | StrobeMode::LowLevel => {
                unreachable!("level-triggered fragments have no repaired boundary")
            }
        };
        if triggered {
            if boundary.cs_eligible {
                merge_sample(
                    boundary.position,
                    boundary.data_value,
                    boundary.reset_before,
                )?;
                boundary_reset = false;
            } else {
                boundary_reset = true;
            }
        }
    }
    for (index, (&position, &value)) in buffers.positions.iter().zip(&buffers.values).enumerate() {
        let reset_before = boundary_reset || buffers.reset_before[index];
        boundary_reset = false;
        merge_sample(position, value, reset_before)?;
    }
    if boundary_reset || fragment.reset_after {
        assembly.cycles = 0;
        assembly.value = 0;
    }
    *previous_strobe_value = fragment.last_strobe_value;
    Ok(words_emitted)
}

fn acquire_stream_block_set(
    strobe_input: &mut Receiver<'_, SampleBlock>,
    data_inputs: &mut [Receiver<'_, SampleBlock>],
    cs_input: &mut Option<Receiver<'_, SampleBlock>>,
    blocks: &mut StreamBlockState,
) -> WorkResult<()> {
    let strobe = strobe_input.recv()?;
    if strobe.num_samples == 0 {
        return Err(WorkError::NodeError(
            "Strobe block must contain at least one sample".to_string(),
        ));
    }
    let aligned = |block: &SampleBlock, input_name: &str| -> WorkResult<()> {
        if block.start_position != strobe.start_position
            || block.num_samples != strobe.num_samples
            || block.timestamp_step != strobe.timestamp_step
        {
            return Err(WorkError::NodeError(format!(
                "{input_name} block misaligned: start={}, samples={}, step={} vs \
                 strobe start={}, samples={}, step={}",
                block.start_position,
                block.num_samples,
                block.timestamp_step,
                strobe.start_position,
                strobe.num_samples,
                strobe.timestamp_step
            )));
        }
        Ok(())
    };

    let mut data = Vec::with_capacity(data_inputs.len());
    for (index, input) in data_inputs.iter_mut().enumerate() {
        let block = input.recv()?;
        aligned(&block, &format!("Data {index}"))?;
        data.push(block);
    }
    let cs = match cs_input {
        Some(input) => {
            let block = input.recv()?;
            aligned(&block, "CS")?;
            Some(block)
        }
        None => None,
    };

    blocks.strobe = Some(strobe);
    blocks.data = data;
    blocks.cs = cs;
    blocks.offset = 0;
    Ok(())
}

/// Enable-signal state, threaded through `process_trigger` calls. Either a
/// point-queried channel (`query`, when the connection negotiated
/// `EdgeQuery`) or a streamed `Sample` level advanced edge by edge.
struct EnableState<'a> {
    current: bool,
    next_change_position: u64,
    input: Option<Receiver<'a, Sample>>,
}

impl ParallelDecoder {
    /// Per-`work()`-call trigger budget for the index-driven path. Each
    /// trigger costs a handful of index queries (microseconds), so this
    /// keeps a call's worst-case duration in the low milliseconds — short
    /// enough that the manager's between-calls stop check makes the node
    /// feel instantly stoppable, long enough that the per-call overhead is
    /// noise.
    const QUERY_TRIGGERS_PER_CALL: usize = 65_536;

    /// Maximum number of packed samples scanned in one streamed-path
    /// `work()` call. The resident blocks remain borrowed through shared
    /// `Arc` storage; only this cursor advances between calls.
    const STREAM_SAMPLES_PER_CALL: usize = 65_536;

    /// Index-driven path: strobe triggers are located by direct skip-ahead
    /// queries instead of a per-sample block scan; data/CS are point-read
    /// at each trigger. `enable_signal` is point-read too when its
    /// connection negotiated `EdgeQuery`, and streams otherwise. Each call
    /// processes at most [`Self::QUERY_TRIGGERS_PER_CALL`] triggers, then
    /// returns with its cursor persisted — there's no natural per-call
    /// window the way SpiDecoder has CS transactions, and an uncapped call
    /// would scan every remaining trigger in the file. That matters beyond
    /// fairness: a fully gated-off stretch (CS active / enable low) makes
    /// no channel calls at all, so a `work()` call stuck in it can only be
    /// interrupted *between* calls — the manager's stop flag, which the UI
    /// relies on to stop a run without freezing.
    #[allow(clippy::too_many_arguments)]
    fn work_indexed(
        &mut self,
        inputs: &[InputPort],
        outputs: &[OutputPort],
        strobe_query: Arc<dyn EdgeQuery>,
        data_queries: Vec<Arc<dyn EdgeQuery>>,
        cs_query: Option<Arc<dyn EdgeQuery>>,
        buffers: &mut QueryBuffers,
    ) -> WorkResult<usize> {
        self.work_call_count += 1;

        // An unconnected output is valid: the node still advances its
        // decoder state, which is useful for optional viewer branches and
        // for measuring decode cost independently from event transport.
        let output = outputs.first().and_then(|port| port.get::<Word>());

        let enable_port_idx = 1 + self.num_data_bits + 1;
        let enable_query = inputs
            .get(enable_port_idx)
            .and_then(|port| port.edge_query());
        // An EdgeQuery-negotiated enable never has a channel — don't let
        // the missing receiver read as "unconnected → always enabled".
        let enable_recv = if enable_query.is_some() {
            None
        } else {
            inputs
                .get(enable_port_idx)
                .and_then(|port| port.get::<Sample>(&mut self.enable_buffer))
        };
        let mut enable = EnableState {
            current: self.current_enable_value,
            next_change_position: self.next_enable_change_position,
            input: enable_recv,
        };
        if enable_query.is_none() && enable.input.is_none() {
            enable.current = true;
            enable.next_change_position = u64::MAX;
        }

        let mode = self.mode;
        let cs_polarity = self.cs_polarity;
        let cycles_per_word = self.cycles_per_word;
        let endianness = self.endianness;
        let num_data_bits = self.num_data_bits;
        let timestamp_step = (1_000_000_000.0 / strobe_query.samplerate_hz()) as u64;
        let total_samples = strobe_query.total_samples();
        // EdgeQuery methods return signal_processing::Result, not WorkResult.
        let query_err = |e: signal_processing::Error| WorkError::NodeError(e.to_string());

        let mut assembly = AssemblyState {
            value: self.assembly_value,
            cycles: self.assembly_cycles,
            first_ts: self.assembly_first_ts,
        };

        buffers.edges.clear();
        buffers.positions.clear();
        let mut raw_position = self.query_strobe_position;

        // Mirrors work_streamed's `last_strobe_value` starting `false`: a
        // strobe already at the triggering level at position 0 counts as a
        // trigger there too (there's no real "edge" at the very first
        // sample — the implicit prior sample is treated as low, exactly
        // like the streaming per-sample scan does).
        if raw_position == 0 {
            let value = strobe_query.value_at(0).map_err(query_err)?;
            let triggered0 = match mode {
                StrobeMode::RisingEdge | StrobeMode::AnyEdge => value,
                StrobeMode::FallingEdge => false,
                StrobeMode::HighLevel | StrobeMode::LowLevel => {
                    unreachable!("excluded from query mode by work()'s sparse_mode gate")
                }
            };
            if triggered0 {
                buffers.positions.push(0);
            }
        }

        let remaining = Self::QUERY_TRIGGERS_PER_CALL - buffers.positions.len();
        let max_edges = match mode {
            StrobeMode::AnyEdge => remaining,
            StrobeMode::RisingEdge | StrobeMode::FallingEdge => remaining.saturating_mul(2),
            StrobeMode::HighLevel | StrobeMode::LowLevel => {
                unreachable!("excluded from query mode by work()'s sparse_mode gate")
            }
        };
        strobe_query
            .next_edges(raw_position, total_samples, max_edges, &mut buffers.edges)
            .map_err(query_err)?;
        let exhausted = buffers.edges.len() < max_edges;
        if let Some(edge) = buffers.edges.last() {
            raw_position = edge.sample;
        }
        buffers.positions.extend(
            buffers
                .edges
                .iter()
                .filter(|edge| match mode {
                    StrobeMode::RisingEdge => edge.value,
                    StrobeMode::FallingEdge => !edge.value,
                    StrobeMode::AnyEdge => true,
                    StrobeMode::HighLevel | StrobeMode::LowLevel => false,
                })
                .map(|edge| edge.sample)
                .take(remaining),
        );

        if cs_polarity != CsPolarity::Disabled {
            let query = cs_query.as_ref().ok_or_else(|| {
                WorkError::NodeError("CS query missing for active polarity".to_string())
            })?;
            query
                .values_at(&buffers.positions, &mut buffers.cs_values)
                .map_err(query_err)?;
        } else {
            buffers.cs_values.clear();
        }

        if let Some(query) = &enable_query {
            query
                .values_at(&buffers.positions, &mut buffers.enable_values)
                .map_err(query_err)?;
        } else {
            buffers.enable_values.clear();
        }

        // Apply gating before touching the data channels. reset_before
        // preserves assembly semantics when a gated trigger separates two
        // eligible triggers whose data values are read later as one batch.
        buffers.eligible_positions.clear();
        buffers.reset_before.clear();
        let mut reset_before_next = false;
        for (trigger_index, &position) in buffers.positions.iter().enumerate() {
            let cs_active = match cs_polarity {
                CsPolarity::ActiveLow => !buffers.cs_values[trigger_index],
                CsPolarity::ActiveHigh => buffers.cs_values[trigger_index],
                CsPolarity::Disabled => true,
            };
            if !cs_active {
                reset_before_next = true;
                continue;
            }

            let timestamp_ns = position.saturating_mul(timestamp_step);
            if enable_query.is_some() {
                enable.current = buffers.enable_values[trigger_index];
            } else if timestamp_ns >= enable.next_change_position
                && let Some(enable_recv) = &mut enable.input
            {
                loop {
                    match enable_recv.peek() {
                        Ok(next_edge) if next_edge.start_time_ns <= timestamp_ns => {
                            enable.current = enable_recv.recv()?.value;
                        }
                        Ok(next_edge) => {
                            enable.next_change_position = next_edge.start_time_ns;
                            break;
                        }
                        Err(WorkError::Shutdown) => {
                            enable.next_change_position = u64::MAX;
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            if !enable.current {
                reset_before_next = true;
                continue;
            }

            buffers.eligible_positions.push(position);
            buffers.reset_before.push(reset_before_next);
            reset_before_next = false;
        }

        buffers.data_values.resize_with(num_data_bits, Vec::new);
        for (query, values) in data_queries.iter().zip(&mut buffers.data_values) {
            query
                .values_at(&buffers.eligible_positions, values)
                .map_err(query_err)?;
        }

        let mut words_emitted = 0u64;
        let mut word_batch = output
            .as_ref()
            .map(|_| Vec::with_capacity(buffers.eligible_positions.len()));
        for (trigger_index, &position) in buffers.eligible_positions.iter().enumerate() {
            if buffers.reset_before[trigger_index] {
                assembly.cycles = 0;
                assembly.value = 0;
            }
            let timestamp_ns = position.saturating_mul(timestamp_step);
            let mut value = 0u64;
            for (bit, values) in buffers.data_values.iter().enumerate() {
                if values[trigger_index] {
                    value |= 1 << bit;
                }
            }
            if assembly.cycles == 0 {
                assembly.first_ts = timestamp_ns;
            }
            match endianness {
                Endianness::Little => {
                    assembly.value |= value << (assembly.cycles * num_data_bits);
                }
                Endianness::Big => {
                    assembly.value = (assembly.value << num_data_bits) | value;
                }
            }
            assembly.cycles += 1;
            if assembly.cycles < cycles_per_word {
                continue;
            }

            if let Some(batch) = &mut word_batch {
                batch.push(Word::spanning(
                    assembly.value,
                    assembly.first_ts,
                    timestamp_ns.saturating_sub(assembly.first_ts),
                ));
            }
            assembly.value = 0;
            assembly.cycles = 0;
            words_emitted += 1;
        }
        if reset_before_next {
            assembly.cycles = 0;
            assembly.value = 0;
        }

        self.query_strobe_position = raw_position;
        self.current_enable_value = enable.current;
        self.next_enable_change_position = enable.next_change_position;
        self.total_words_emitted += words_emitted;
        self.assembly_value = assembly.value;
        self.assembly_cycles = assembly.cycles;
        self.assembly_first_ts = assembly.first_ts;
        if let (Some(output), Some(batch)) = (&output, word_batch)
            && !batch.is_empty()
        {
            output.send_batch(batch)?;
        }

        debug!(
            "[{}] Query-mode batch {} done: {} words, {} total",
            self.name, self.work_call_count, words_emitted, self.total_words_emitted
        );

        if exhausted {
            Err(WorkError::Shutdown)
        } else {
            Ok(words_emitted as usize)
        }
    }

    /// Streaming path: unchanged behavior for live sources or any
    /// connection that didn't negotiate `EdgeQuery`.
    fn work_streamed(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut blocks = std::mem::take(&mut self.stream_blocks);
        let result = worker_backend::work(self, inputs, outputs, &mut blocks);
        self.stream_blocks = blocks;
        result
    }

    fn work_streamed_inner(
        &mut self,
        inputs: &[InputPort],
        outputs: &[OutputPort],
        blocks: &mut StreamBlockState,
    ) -> WorkResult<usize> {
        self.work_call_count += 1;

        if self.work_call_count == 1 {
            debug!(
                "[{}] First work() call: {} inputs, {} outputs",
                self.name,
                inputs.len(),
                outputs.len()
            );
        }

        // Keep decoding when the output is intentionally unconnected.
        let output = outputs.first().and_then(|port| port.get::<Word>());

        // Get block inputs: strobe, data[0..N], cs
        let mut strobe_buf = VecDeque::new();
        let mut strobe_input = inputs
            .first()
            .and_then(|port| port.get::<SampleBlock>(&mut strobe_buf))
            .ok_or_else(|| WorkError::NodeError("Missing strobe block input".to_string()))?;

        let mut data_bufs: Vec<VecDeque<SampleBlock>> =
            (0..self.num_data_bits).map(|_| VecDeque::new()).collect();
        let mut data_inputs: Vec<Receiver<'_, SampleBlock>> =
            Vec::with_capacity(self.num_data_bits);
        for (i, buf) in data_bufs.iter_mut().enumerate() {
            let input = inputs
                .get(1 + i)
                .and_then(|port| port.get::<SampleBlock>(buf))
                .ok_or_else(|| WorkError::NodeError(format!("Missing data block input {}", i)))?;
            data_inputs.push(input);
        }

        // CS is optional when the polarity is Disabled (unconnected input →
        // dummy port → get() returns None). When connected it is received
        // every iteration regardless of polarity to stay in block lockstep.
        let mut cs_buf = VecDeque::new();
        let mut cs_input = inputs
            .get(1 + self.num_data_bits)
            .and_then(|port| port.get::<SampleBlock>(&mut cs_buf));
        if cs_input.is_none() && self.cs_polarity != CsPolarity::Disabled {
            return Err(WorkError::NodeError(
                "CS input unconnected but CS polarity is not Disabled".to_string(),
            ));
        }

        // Get edge input: enable_signal — optional; unconnected means
        // always enabled. When its connection negotiated EdgeQuery there
        // is no channel at all — point-read it per trigger instead (this
        // can happen even while the block inputs stream, e.g. a level
        // strobe mode keeping query mode off).
        // Use local variables for enable state to avoid borrowing self
        let enable_port_idx = 1 + self.num_data_bits + 1;
        let enable_query = inputs
            .get(enable_port_idx)
            .and_then(|port| port.edge_query());
        let mut current_enable_value = self.current_enable_value;
        let mut enable_input = if enable_query.is_some() {
            None
        } else {
            inputs
                .get(enable_port_idx)
                .and_then(|port| port.get::<Sample>(&mut self.enable_buffer))
        };
        if enable_query.is_none() && enable_input.is_none() {
            current_enable_value = true;
        }

        // Acquire the next aligned block set only after the previous set is
        // completely consumed. SampleBlock's Arc-backed payload stays in
        // this state while bounded windows advance through it, so no packed
        // bytes are copied or split for scheduler fairness.
        if blocks.strobe.is_none() {
            acquire_stream_block_set(&mut strobe_input, &mut data_inputs, &mut cs_input, blocks)?;
        }

        let strobe_block = blocks.strobe.as_ref().expect("block set acquired above");
        let data_blocks = &blocks.data;
        let cs_block = &blocks.cs;

        let num_samples = strobe_block.num_samples;
        let window_start = blocks.offset;
        let window_end = window_start
            .saturating_add(Self::STREAM_SAMPLES_PER_CALL)
            .min(num_samples);
        let enabled_ranges = enabled_ranges_for_window(
            enable_query.as_ref(),
            &mut enable_input,
            &mut current_enable_value,
            strobe_block.start_position,
            window_start,
            window_end,
            strobe_block.timestamp_step,
        )?;
        let mut fragment = scan_stream_fragment(
            StreamScanConfig {
                mode: self.mode,
                cs_polarity: self.cs_polarity,
            },
            blocks.next_sequence,
            strobe_block,
            data_blocks,
            cs_block.as_ref(),
            window_start,
            window_end,
            &enabled_ranges,
            std::mem::take(&mut blocks.fragment_buffers),
        );
        if fragment.sequence != self.next_stream_merge_sequence {
            return Err(WorkError::NodeError(format!(
                "Out-of-order decode fragment: expected sequence {}, received {}",
                self.next_stream_merge_sequence, fragment.sequence
            )));
        }

        let mut word_batch = output
            .as_ref()
            .map(|_| Vec::with_capacity(fragment.buffers.positions.len()));
        let mut assembly = AssemblyState {
            value: self.assembly_value,
            cycles: self.assembly_cycles,
            first_ts: self.assembly_first_ts,
        };
        let mut last_strobe_value = self.last_strobe_value;
        let words_emitted = merge_stream_fragment(
            &fragment,
            self.mode,
            &mut last_strobe_value,
            self.num_data_bits,
            self.cycles_per_word,
            self.endianness,
            &mut assembly,
            &mut word_batch,
        )?;
        blocks.next_sequence += 1;
        self.next_stream_merge_sequence += 1;

        // Save state back
        self.last_strobe_value = last_strobe_value;
        self.current_enable_value = current_enable_value;
        self.total_words_emitted += words_emitted;
        self.assembly_value = assembly.value;
        self.assembly_cycles = assembly.cycles;
        self.assembly_first_ts = assembly.first_ts;
        blocks.fragment_buffers = std::mem::take(&mut fragment.buffers);

        blocks.offset = window_end;
        if window_end == num_samples {
            blocks.strobe = None;
            blocks.data.clear();
            blocks.cs = None;
            blocks.offset = 0;
        }
        if let (Some(output), Some(batch)) = (&output, word_batch)
            && !batch.is_empty()
        {
            output.send_batch(batch)?;
        }

        if self.work_call_count.is_multiple_of(10) || words_emitted > 0 {
            debug!(
                "[{}] Stream window {} done: {} words this window, {} total",
                self.name, fragment.sequence, words_emitted, self.total_words_emitted
            );
        }

        Ok(words_emitted as usize)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crossbeam_channel::bounded;

    use signal_processing::node::ProcessNode;
    use signal_processing::sender::{ChannelMessage, Sender};
    use signal_processing::watchdog::Watchdog;

    use super::*;

    #[test]
    fn test_decoder_creation() {
        let decoder = ParallelDecoder::new(8, StrobeMode::RisingEdge, CsPolarity::ActiveLow);
        assert_eq!(decoder.num_data_bits, 8);
        assert_eq!(decoder.cs_polarity, CsPolarity::ActiveLow);
        // Block inputs: strobe + 8 data + cs = 10, Edge input: enable = 1, Total = 11
        assert_eq!(decoder.num_inputs(), 11);
        assert_eq!(
            decoder.input_schema()[0].protocols,
            vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream]
        );
    }

    #[test]
    fn input_strategy_constrains_raw_signal_protocols() {
        let packed = ParallelDecoder::new(2, StrobeMode::AnyEdge, CsPolarity::Disabled)
            .with_input_strategy(ParallelInputStrategy::PackedStream);
        let indexed = ParallelDecoder::new(2, StrobeMode::AnyEdge, CsPolarity::Disabled)
            .with_input_strategy(ParallelInputStrategy::Indexed);

        for schema in &packed.input_schema()[..4] {
            assert_eq!(schema.protocols, vec![ProtocolKind::Stream]);
        }
        for schema in &indexed.input_schema()[..4] {
            assert_eq!(schema.protocols, vec![ProtocolKind::EdgeQuery]);
        }
        assert_eq!(
            packed.input_schema()[4].protocols,
            vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream],
            "the enable level chooses its transport independently"
        );
    }

    #[test]
    fn level_trigger_forces_packed_streaming() {
        let decoder = ParallelDecoder::new(1, StrobeMode::HighLevel, CsPolarity::Disabled)
            .with_input_strategy(ParallelInputStrategy::Indexed);
        for schema in &decoder.input_schema()[..3] {
            assert_eq!(schema.protocols, vec![ProtocolKind::Stream]);
        }
    }

    struct DensityQuery(f64);

    impl EdgeQuery for DensityQuery {
        fn sample_period(&self) -> f64 {
            1.0
        }
        fn samplerate_hz(&self) -> f64 {
            1.0
        }
        fn total_samples(&self) -> u64 {
            1
        }
        fn activity_ratio_hint(&self) -> Option<f64> {
            Some(self.0)
        }
        fn value_at(&self, _position: u64) -> signal_processing::Result<bool> {
            Ok(false)
        }
        fn next_edge(
            &self,
            _position: u64,
            _limit: u64,
        ) -> signal_processing::Result<Option<CaptureTransition>> {
            Ok(None)
        }
    }

    fn auto_candidates(activity: f64) -> Vec<Option<InputProtocolCandidate>> {
        let query: Arc<dyn EdgeQuery> = Arc::new(DensityQuery(activity));
        (0..5)
            .map(|_| {
                Some(InputProtocolCandidate {
                    offered: vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream],
                    edge_query: Some(Arc::clone(&query)),
                })
            })
            .collect()
    }

    #[test]
    fn auto_strategy_selects_one_protocol_for_the_complete_raw_group() {
        let decoder = ParallelDecoder::new(2, StrobeMode::AnyEdge, CsPolarity::Disabled);
        let dense = decoder.select_input_protocols(&auto_candidates(0.9));
        let sparse = decoder.select_input_protocols(&auto_candidates(0.01));

        assert_eq!(&dense[..4], &[Some(ProtocolKind::Stream); 4]);
        assert_eq!(&sparse[..4], &[Some(ProtocolKind::EdgeQuery); 4]);
        assert_eq!(
            dense[4],
            Some(ProtocolKind::EdgeQuery),
            "enable transport remains independent"
        );
    }

    #[test]
    fn auto_strategy_falls_back_as_a_group_when_one_input_lacks_preferred_protocol() {
        let decoder = ParallelDecoder::new(2, StrobeMode::AnyEdge, CsPolarity::Disabled);
        let mut candidates = auto_candidates(0.01);
        candidates[1].as_mut().unwrap().offered = vec![ProtocolKind::Stream];

        let selected = decoder.select_input_protocols(&candidates);
        assert_eq!(&selected[..4], &[Some(ProtocolKind::Stream); 4]);
    }

    fn block_from_bits(bits: &[bool]) -> SampleBlock {
        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (i, &bit) in bits.iter().enumerate() {
            if bit {
                bytes[i / 8] |= 1 << (i % 8);
            }
        }
        SampleBlock::new(Arc::from(bytes.into_boxed_slice()), 0, bits.len(), 1)
    }

    fn block_input(wd: &Watchdog, block: SampleBlock, name: &str) -> InputPort {
        let (tx, rx) = bounded::<ChannelMessage<SampleBlock>>(4);
        tx.send(ChannelMessage::Sample(block)).unwrap();
        drop(tx);
        InputPort::new_with_watchdog(rx, wd, "pd", name)
    }

    fn unconnected(wd: &Watchdog, name: &str) -> InputPort {
        InputPort::disconnected().with_watchdog(wd.clone(), "pd".to_string(), name.to_string())
    }

    fn collect_messages<T>(receiver: crossbeam_channel::Receiver<ChannelMessage<T>>) -> Vec<T> {
        let mut collected = Vec::new();
        for message in receiver.try_iter() {
            match message {
                ChannelMessage::Sample(item) => collected.push(item),
                ChannelMessage::Batch(items) => collected.extend(items),
                ChannelMessage::EndOfStream => {}
            }
        }
        collected
    }

    /// 4-bit bus, strobe rising at positions 1,5,9,13, bus values 1,2,3,4.
    /// CS and enable are left unconnected.
    fn run_4bit(decoder: &mut ParallelDecoder) -> Vec<Word> {
        let wd = Watchdog::new();
        let n = 16usize;
        let values = [0u64, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4];
        let strobe: Vec<bool> = (0..n).map(|i| i % 4 == 1 || i % 4 == 2).collect();
        let mut inputs = vec![block_input(&wd, block_from_bits(&strobe), "strobe")];
        for bit in 0..4 {
            let bits: Vec<bool> = (0..n).map(|i| (values[i] >> bit) & 1 == 1).collect();
            inputs.push(block_input(&wd, block_from_bits(&bits), &format!("d{bit}")));
        }
        inputs.push(unconnected(&wd, "cs"));
        inputs.push(unconnected(&wd, "enable_signal"));

        let (out_tx, out_rx) = bounded::<ChannelMessage<Word>>(64);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "pd",
            "words",
        )];

        loop {
            match decoder.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        collect_messages(out_rx)
    }

    #[test]
    fn unconnected_cs_and_enable_pass_through() {
        let mut decoder = ParallelDecoder::new(4, StrobeMode::RisingEdge, CsPolarity::Disabled);
        let words = run_4bit(&mut decoder);
        assert_eq!(
            words.iter().map(|w| w.value).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        // Timestamps at the strobe positions (step = 1ns).
        assert_eq!(
            words.iter().map(|w| w.timestamp_ns).collect::<Vec<_>>(),
            vec![1, 5, 9, 13]
        );
    }

    #[test]
    fn unconnected_cs_with_active_polarity_is_an_error() {
        let wd = Watchdog::new();
        let mut decoder = ParallelDecoder::new(1, StrobeMode::RisingEdge, CsPolarity::ActiveLow);
        let inputs = [
            block_input(&wd, block_from_bits(&[false, true]), "strobe"),
            block_input(&wd, block_from_bits(&[true, true]), "d0"),
            unconnected(&wd, "cs"),
            unconnected(&wd, "enable_signal"),
        ];
        let (out_tx, _out_rx) = bounded::<ChannelMessage<Word>>(4);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "pd",
            "words",
        )];
        assert!(matches!(
            decoder.work(&inputs, &outputs),
            Err(WorkError::NodeError(_))
        ));
    }

    #[test]
    fn unconnected_output_still_advances_decoder() {
        let wd = Watchdog::new();
        let inputs = [
            block_input(&wd, block_from_bits(&[false, true]), "strobe"),
            block_input(&wd, block_from_bits(&[true, true]), "d0"),
            unconnected(&wd, "cs"),
            unconnected(&wd, "enable_signal"),
        ];
        let outputs: [OutputPort; 0] = [];
        let mut decoder = ParallelDecoder::new(1, StrobeMode::RisingEdge, CsPolarity::Disabled);

        assert_eq!(decoder.work(&inputs, &outputs).unwrap(), 1);
        assert_eq!(decoder.total_words_emitted, 1);
        assert!(matches!(
            decoder.work(&inputs, &outputs),
            Err(WorkError::Shutdown)
        ));
    }

    fn run_1bit_mode(strobe: &[bool], mode: StrobeMode) -> Vec<Word> {
        let wd = Watchdog::new();
        let data: Vec<bool> = (0..strobe.len())
            .map(|position| position % 3 == 1)
            .collect();
        let inputs = [
            block_input(&wd, block_from_bits(strobe), "strobe"),
            block_input(&wd, block_from_bits(&data), "d0"),
            unconnected(&wd, "cs"),
            unconnected(&wd, "enable_signal"),
        ];
        let (out_tx, out_rx) = bounded::<ChannelMessage<Word>>(strobe.len() + 1);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "pd",
            "words",
        )];
        let mut decoder = ParallelDecoder::new(1, mode, CsPolarity::Disabled);

        loop {
            match decoder.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(error) => panic!("unexpected error: {error}"),
            }
        }
        collect_messages(out_rx)
    }

    #[test]
    fn packed_scan_matches_scalar_trigger_semantics() {
        let strobe: Vec<bool> = (0..137)
            .map(|position| matches!(position, 0..=2 | 7..=63 | 65..=66 | 128..=136))
            .collect();
        for mode in [
            StrobeMode::RisingEdge,
            StrobeMode::FallingEdge,
            StrobeMode::AnyEdge,
            StrobeMode::HighLevel,
            StrobeMode::LowLevel,
        ] {
            let mut previous = false;
            let expected_positions: Vec<u64> = strobe
                .iter()
                .enumerate()
                .filter_map(|(position, &value)| {
                    let triggered = match mode {
                        StrobeMode::RisingEdge => !previous && value,
                        StrobeMode::FallingEdge => previous && !value,
                        StrobeMode::AnyEdge => previous != value,
                        StrobeMode::HighLevel => value,
                        StrobeMode::LowLevel => !value,
                    };
                    previous = value;
                    triggered.then_some(position as u64)
                })
                .collect();
            let words = run_1bit_mode(&strobe, mode);
            let actual_positions: Vec<_> = words.iter().map(|word| word.timestamp_ns).collect();
            assert_eq!(actual_positions, expected_positions, "mode={mode:?}");
            for word in words {
                assert_eq!(word.value, u64::from(word.timestamp_ns % 3 == 1));
            }
        }
    }

    fn merge_fragments_for_test(
        mode: StrobeMode,
        fragments: &[DecodeFragment],
    ) -> (Vec<Word>, AssemblyState) {
        let mut assembly = AssemblyState {
            value: 0,
            cycles: 0,
            first_ts: 0,
        };
        let mut words = Some(Vec::new());
        let mut previous_strobe_value = false;

        for fragment in fragments {
            merge_stream_fragment(
                fragment,
                mode,
                &mut previous_strobe_value,
                3,
                3,
                Endianness::Little,
                &mut assembly,
                &mut words,
            )
            .unwrap();
        }

        (words.unwrap(), assembly)
    }

    #[test]
    fn fragment_splits_preserve_edges_resets_and_partial_words() {
        let sample_count = 137usize;
        let strobe_bits: Vec<bool> = (0..sample_count)
            .map(|position| position % 4 == 1 || position % 4 == 2)
            .collect();
        let bus_values: Vec<u64> = (0..sample_count)
            .map(|position| ((position * 5 + 3) & 0b111) as u64)
            .collect();
        let data_blocks: Vec<SampleBlock> = (0..3)
            .map(|bit| {
                block_from_bits(
                    &bus_values
                        .iter()
                        .map(|value| value & (1 << bit) != 0)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        // Active-low CS is eligible only in these low spans. The surrounding
        // high spans gate triggers and reset partial words across many
        // possible fragment boundaries.
        let cs_bits: Vec<bool> = (0..sample_count)
            .map(|position| !matches!(position, 29..=38 | 84..=90 | 128..=136))
            .collect();
        let strobe = block_from_bits(&strobe_bits);
        let cs = block_from_bits(&cs_bits);

        for mode in [
            StrobeMode::RisingEdge,
            StrobeMode::FallingEdge,
            StrobeMode::AnyEdge,
            StrobeMode::HighLevel,
            StrobeMode::LowLevel,
        ] {
            let config = StreamScanConfig {
                mode,
                cs_polarity: CsPolarity::ActiveLow,
            };
            let whole = scan_stream_fragment(
                config,
                0,
                &strobe,
                &data_blocks,
                Some(&cs),
                0,
                sample_count,
                &[EnabledRange {
                    start: 0,
                    end: sample_count,
                    reset_before: false,
                }],
                DecodeFragmentBuffers::default(),
            );
            let (expected_words, expected_assembly) = merge_fragments_for_test(mode, &[whole]);

            for split in 1..sample_count {
                let first = scan_stream_fragment(
                    config,
                    0,
                    &strobe,
                    &data_blocks,
                    Some(&cs),
                    0,
                    split,
                    &[EnabledRange {
                        start: 0,
                        end: split,
                        reset_before: false,
                    }],
                    DecodeFragmentBuffers::default(),
                );
                let second = scan_stream_fragment(
                    config,
                    1,
                    &strobe,
                    &data_blocks,
                    Some(&cs),
                    split,
                    sample_count,
                    &[EnabledRange {
                        start: split,
                        end: sample_count,
                        reset_before: false,
                    }],
                    DecodeFragmentBuffers::default(),
                );
                let (actual_words, actual_assembly) =
                    merge_fragments_for_test(mode, &[first, second]);

                assert_eq!(actual_words, expected_words, "mode={mode:?}, split={split}");
                assert_eq!(
                    (
                        actual_assembly.value,
                        actual_assembly.cycles,
                        actual_assembly.first_ts
                    ),
                    (
                        expected_assembly.value,
                        expected_assembly.cycles,
                        expected_assembly.first_ts
                    ),
                    "mode={mode:?}, split={split}"
                );
            }
        }
    }

    #[test]
    fn streamed_block_is_retained_and_processed_in_bounded_windows() {
        let wd = Watchdog::new();
        let sample_count = 2 * ParallelDecoder::STREAM_SAMPLES_PER_CALL + 10;
        let backing: Arc<[u8]> = Arc::from(vec![0u8; sample_count.div_ceil(8)].into_boxed_slice());
        let shared_backing = signal_processing::capture::BlockData::from(backing);
        let strobe = SampleBlock::new(shared_backing.clone(), 0, sample_count, 1);
        let inputs = [
            block_input(&wd, strobe, "strobe"),
            block_input(&wd, block_from_bits(&vec![false; sample_count]), "d0"),
            unconnected(&wd, "cs"),
            unconnected(&wd, "enable_signal"),
        ];
        let (out_tx, out_rx) = bounded::<ChannelMessage<Word>>(4);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "pd",
            "words",
        )];
        let mut decoder = ParallelDecoder::new(1, StrobeMode::AnyEdge, CsPolarity::Disabled)
            .with_parallel_workers(1);

        assert_eq!(decoder.work(&inputs, &outputs).unwrap(), 0);
        let resident = decoder.stream_blocks.strobe.as_ref().unwrap();
        assert!(shared_backing.shares_backing(&resident.data));
        assert_eq!(
            decoder.stream_blocks.offset,
            ParallelDecoder::STREAM_SAMPLES_PER_CALL
        );
        assert_eq!(decoder.work(&inputs, &outputs).unwrap(), 0);
        assert_eq!(decoder.work(&inputs, &outputs).unwrap(), 0);
        assert!(decoder.stream_blocks.strobe.is_none());
        assert!(matches!(
            decoder.work(&inputs, &outputs),
            Err(WorkError::Shutdown)
        ));
        assert_eq!(out_rx.try_iter().count(), 0);
    }

    #[test]
    fn word_assembly_little_endian() {
        let mut decoder = ParallelDecoder::new(4, StrobeMode::RisingEdge, CsPolarity::Disabled)
            .with_word_assembly(2, Endianness::Little);
        let words = run_4bit(&mut decoder);
        assert_eq!(
            words.iter().map(|w| w.value).collect::<Vec<_>>(),
            vec![0x21, 0x43]
        );
        // Word timestamped at its first cycle.
        assert_eq!(
            words.iter().map(|w| w.timestamp_ns).collect::<Vec<_>>(),
            vec![1, 9]
        );
    }

    #[test]
    fn word_assembly_big_endian() {
        let mut decoder = ParallelDecoder::new(4, StrobeMode::RisingEdge, CsPolarity::Disabled)
            .with_word_assembly(2, Endianness::Big);
        let words = run_4bit(&mut decoder);
        assert_eq!(
            words.iter().map(|w| w.value).collect::<Vec<_>>(),
            vec![0x12, 0x34]
        );
    }

    /// Minimal in-memory [`EdgeQuery`] over a literal sample vector:
    /// 1 GHz (position == nanosecond), matching `block_from_bits`'
    /// `timestamp_step = 1`.
    struct FakeChannel {
        bits: Vec<bool>,
    }

    impl EdgeQuery for FakeChannel {
        fn sample_period(&self) -> f64 {
            1e-9
        }
        fn samplerate_hz(&self) -> f64 {
            1e9
        }
        fn total_samples(&self) -> u64 {
            self.bits.len() as u64
        }
        fn value_at(&self, position: u64) -> signal_processing::Result<bool> {
            Ok(self.bits[position as usize])
        }
        fn next_edge(
            &self,
            position: u64,
            limit: u64,
        ) -> signal_processing::Result<Option<signal_processing::capture::CaptureTransition>>
        {
            let mut current = self.bits[position as usize];
            for p in (position + 1)..limit.min(self.total_samples()) {
                let value = self.bits[p as usize];
                if value != current {
                    return Ok(Some(signal_processing::capture::CaptureTransition {
                        sample: p,
                        value,
                    }));
                }
                current = value;
            }
            Ok(None)
        }
    }

    struct CountingChannel {
        bits: Vec<bool>,
        reads: Arc<AtomicUsize>,
    }

    impl EdgeQuery for CountingChannel {
        fn sample_period(&self) -> f64 {
            1e-9
        }
        fn samplerate_hz(&self) -> f64 {
            1e9
        }
        fn total_samples(&self) -> u64 {
            self.bits.len() as u64
        }
        fn value_at(&self, position: u64) -> signal_processing::Result<bool> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.bits[position as usize])
        }
        fn next_edge(
            &self,
            _position: u64,
            _limit: u64,
        ) -> signal_processing::Result<Option<CaptureTransition>> {
            unreachable!("data-only test query must not be searched for edges")
        }
    }

    fn query_input(wd: &Watchdog, bits: &[bool], name: &str) -> InputPort {
        InputPort::disconnected()
            .with_edge_query(Some(Arc::new(FakeChannel {
                bits: bits.to_vec(),
            })))
            .with_watchdog(wd.clone(), "pd".to_string(), name.to_string())
    }

    fn run_1bit_cs_level(query: bool, polarity: CsPolarity, cs_value: bool) -> Vec<Word> {
        let wd = Watchdog::new();
        let strobe = [false, true, false, true];
        let data = [true; 4];
        let cs = [cs_value; 4];
        let input = |bits: &[bool], name: &str| {
            if query {
                query_input(&wd, bits, name)
            } else {
                block_input(&wd, block_from_bits(bits), name)
            }
        };
        let inputs = [
            input(&strobe, "strobe"),
            input(&data, "d0"),
            input(&cs, "cs"),
            unconnected(&wd, "enable_signal"),
        ];
        let (out_tx, out_rx) = bounded::<ChannelMessage<Word>>(4);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "pd",
            "words",
        )];
        let mut decoder = ParallelDecoder::new(1, StrobeMode::RisingEdge, polarity);
        loop {
            match decoder.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(error) => panic!("unexpected error: {error}"),
            }
        }
        collect_messages(out_rx)
    }

    #[test]
    fn cs_polarities_use_the_documented_active_level_in_both_protocols() {
        for query in [false, true] {
            assert_eq!(
                run_1bit_cs_level(query, CsPolarity::ActiveLow, false).len(),
                2,
                "active-low CS must decode while low, query={query}"
            );
            assert!(
                run_1bit_cs_level(query, CsPolarity::ActiveLow, true).is_empty(),
                "active-low CS must gate while high, query={query}"
            );
            assert_eq!(
                run_1bit_cs_level(query, CsPolarity::ActiveHigh, true).len(),
                2,
                "active-high CS must decode while high, query={query}"
            );
            assert!(
                run_1bit_cs_level(query, CsPolarity::ActiveHigh, false).is_empty(),
                "active-high CS must gate while low, query={query}"
            );
        }
    }

    #[test]
    fn query_mode_does_not_read_data_at_gated_triggers() {
        let wd = Watchdog::new();
        let reads = Arc::new(AtomicUsize::new(0));
        let data_query: Arc<dyn EdgeQuery> = Arc::new(CountingChannel {
            bits: vec![true; 4],
            reads: reads.clone(),
        });
        let data_input = InputPort::disconnected()
            .with_edge_query(Some(data_query))
            .with_watchdog(wd.clone(), "pd".to_string(), "d0".to_string());
        let inputs = [
            query_input(&wd, &[false, true, false, true], "strobe"),
            data_input,
            // Active-low CS remains high (inactive), gating every trigger.
            query_input(&wd, &[true; 4], "cs"),
            unconnected(&wd, "enable_signal"),
        ];
        let (out_tx, out_rx) = bounded::<ChannelMessage<Word>>(4);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "pd",
            "words",
        )];
        let mut decoder = ParallelDecoder::new(1, StrobeMode::RisingEdge, CsPolarity::ActiveLow);

        assert!(matches!(
            decoder.work(&inputs, &outputs),
            Err(WorkError::Shutdown)
        ));
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        assert_eq!(out_rx.try_iter().count(), 0);
    }

    #[test]
    fn query_batch_preserves_word_assembly_gating_boundaries() {
        let wd = Watchdog::new();
        let inputs = [
            query_input(
                &wd,
                &[false, true, false, true, false, true, false, true],
                "strobe",
            ),
            query_input(&wd, &[true; 8], "d0"),
            unconnected(&wd, "cs"),
            // Gate off the second trigger at position 3. The incomplete
            // word begun at position 1 must not combine with position 5.
            query_input(
                &wd,
                &[true, true, true, false, true, true, true, true],
                "enable_signal",
            ),
        ];
        let (out_tx, out_rx) = bounded::<ChannelMessage<Word>>(4);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "pd",
            "words",
        )];
        let mut decoder = ParallelDecoder::new(1, StrobeMode::RisingEdge, CsPolarity::Disabled)
            .with_word_assembly(2, Endianness::Little);

        assert!(matches!(
            decoder.work(&inputs, &outputs),
            Err(WorkError::Shutdown)
        ));
        let words = collect_messages(out_rx);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].value, 0b11);
        assert_eq!(words[0].timestamp_ns, 5);
        assert_eq!(words[0].duration_ns, 2);
    }

    /// The 4-bit fixture of `run_4bit`, with an enable signal that is low
    /// until position 4 — so the first word (value 1, strobe at position 1)
    /// must be gated off and only 2@5, 3@9, 4@13 survive. Bus inputs and
    /// the enable can each be wired streamed or query-backed.
    fn run_4bit_gated(query_bus: bool, query_enable: bool) -> Vec<Word> {
        let wd = Watchdog::new();
        let n = 16usize;
        let values = [0u64, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4];
        let strobe: Vec<bool> = (0..n).map(|i| i % 4 == 1 || i % 4 == 2).collect();
        let enable_bits: Vec<bool> = (0..n).map(|i| i >= 4).collect();

        let bus_input = |bits: &[bool], name: &str| -> InputPort {
            if query_bus {
                query_input(&wd, bits, name)
            } else {
                block_input(&wd, block_from_bits(bits), name)
            }
        };
        let mut inputs = vec![bus_input(&strobe, "strobe")];
        for bit in 0..4 {
            let bits: Vec<bool> = (0..n).map(|i| (values[i] >> bit) & 1 == 1).collect();
            inputs.push(bus_input(&bits, &format!("d{bit}")));
        }
        inputs.push(unconnected(&wd, "cs"));
        inputs.push(if query_enable {
            query_input(&wd, &enable_bits, "enable_signal")
        } else {
            // The same level timeline as `enable_bits`, streamed the way a
            // real Sample producer delivers it.
            let (tx, rx) = bounded::<ChannelMessage<Sample>>(8);
            tx.send(ChannelMessage::Sample(Sample::new(false, 0)))
                .unwrap();
            tx.send(ChannelMessage::Sample(Sample::new(true, 4)))
                .unwrap();
            drop(tx);
            InputPort::new_with_watchdog(rx, &wd, "pd", "enable_signal")
        });

        let (out_tx, out_rx) = bounded::<ChannelMessage<Word>>(64);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "pd",
            "words",
        )];

        let mut decoder = ParallelDecoder::new(4, StrobeMode::RisingEdge, CsPolarity::Disabled);
        loop {
            match decoder.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        collect_messages(out_rx)
    }

    #[test]
    fn streamed_enable_builds_watermarked_scan_ranges() {
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<Sample>>(8);
        for (value, timestamp) in [(false, 0), (true, 4), (false, 8), (true, 12)] {
            tx.send(ChannelMessage::Sample(Sample::new(value, timestamp)))
                .unwrap();
        }
        drop(tx);
        let input_port = InputPort::new_with_watchdog(rx, &wd, "pd", "enable_signal");
        let mut buffer = VecDeque::new();
        let mut enable_input = input_port.get::<Sample>(&mut buffer);
        let mut current = false;

        let first =
            enabled_ranges_for_window(None, &mut enable_input, &mut current, 0, 0, 8, 1).unwrap();
        assert_eq!(
            first,
            vec![EnabledRange {
                start: 4,
                end: 8,
                reset_before: true,
            }]
        );

        let second =
            enabled_ranges_for_window(None, &mut enable_input, &mut current, 0, 8, 16, 1).unwrap();
        assert_eq!(
            second,
            vec![EnabledRange {
                start: 12,
                end: 16,
                reset_before: true,
            }]
        );
    }

    #[test]
    fn packed_scan_ignores_clock_toggles_outside_enabled_ranges() {
        let strobe = block_from_bits(
            &(0..16)
                .map(|position| position % 2 == 1)
                .collect::<Vec<_>>(),
        );
        let data = [block_from_bits(&[true; 16])];
        let fragment = scan_stream_fragment(
            StreamScanConfig {
                mode: StrobeMode::AnyEdge,
                cs_polarity: CsPolarity::Disabled,
            },
            0,
            &strobe,
            &data,
            None,
            0,
            16,
            &[
                EnabledRange {
                    start: 4,
                    end: 8,
                    reset_before: true,
                },
                EnabledRange {
                    start: 12,
                    end: 16,
                    reset_before: true,
                },
            ],
            DecodeFragmentBuffers::default(),
        );

        assert_eq!(fragment.buffers.positions, vec![4, 5, 6, 7, 12, 13, 14, 15]);
        assert_eq!(
            fragment.buffers.reset_before,
            vec![true, false, false, false, true, false, false, false]
        );
    }

    /// A long gated-off stretch makes no channel calls, so a `work()` call
    /// stuck in it can only be interrupted between calls — the per-call
    /// trigger budget is what keeps an index-driven run stoppable (and the
    /// UI responsive). More triggers than one budget must take more than
    /// one `work()` call.
    #[test]
    fn query_mode_yields_between_trigger_batches() {
        let wd = Watchdog::new();
        let n = 2 * ParallelDecoder::QUERY_TRIGGERS_PER_CALL + 100;
        // Alternating strobe: every position ≥ 1 is an AnyEdge trigger.
        let strobe: Vec<bool> = (0..n).map(|i| i % 2 == 1).collect();
        let low = vec![false; n];

        let inputs = [
            query_input(&wd, &strobe, "strobe"),
            query_input(&wd, &low, "d0"),
            unconnected(&wd, "cs"),
            // Enable low throughout: everything is gated off, so the scan
            // never touches a channel at all.
            query_input(&wd, &low, "enable_signal"),
        ];
        let (out_tx, out_rx) = bounded::<ChannelMessage<Word>>(4);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "pd",
            "words",
        )];

        let mut decoder = ParallelDecoder::new(1, StrobeMode::AnyEdge, CsPolarity::Disabled);
        let mut ok_calls = 0usize;
        loop {
            match decoder.work(&inputs, &outputs) {
                Ok(_) => ok_calls += 1,
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
            assert!(ok_calls < 100, "budget never exhausted the fixture");
        }
        assert!(
            ok_calls >= 2,
            "expected the scan to yield between trigger batches, got {ok_calls} Ok calls"
        );
        assert_eq!(out_rx.try_iter().count(), 0, "everything was gated off");
    }

    /// Enable gating must behave identically whichever protocol each input
    /// negotiated: fully streamed, mixed either way, or fully query-backed.
    #[test]
    fn enable_gating_is_protocol_independent() {
        let expected: Vec<(u64, u64)> = vec![(2, 5), (3, 9), (4, 13)];
        for (query_bus, query_enable) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let words = run_4bit_gated(query_bus, query_enable);
            let view: Vec<_> = words.iter().map(|w| (w.value, w.timestamp_ns)).collect();
            assert_eq!(
                view, expected,
                "query_bus={query_bus} query_enable={query_enable}"
            );
        }
    }

    // ── Differential test: query-mode output must match streaming-mode ──
}
