//! Parallel bus decoder for block-based processing
//!
//! Accepts SampleBlock inputs for high-bandwidth signals (strobe, data, CS)
//! and Sample inputs for low-bandwidth control signals (enable_signal).
//! Outputs Word events.

use super::types::{CsPolarity, Endianness, StrobeMode};
use crate::runtime::Receiver;
use crate::runtime::WorkError;
use crate::runtime::capture::CaptureTransition;
use crate::runtime::edge_query::EdgeQuery;
use crate::runtime::events::Word;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkResult};
use crate::runtime::protocol::ProtocolKind;
use crate::runtime::sample::{Sample, SampleBlock};
use std::collections::VecDeque;
use std::sync::Arc;
use tracing::debug;

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

    /// Bus cycles assembled into one output word (1 = one cycle per word)
    cycles_per_word: usize,
    /// Cycle order when `cycles_per_word > 1`
    endianness: Endianness,

    /// Putback buffer for enable_signal (edge-based Sample input)
    enable_buffer: VecDeque<Sample>,

    /// Current enable state from edge-based enable_signal
    current_enable_value: bool,
    /// Position up to which the current enable value is known to be valid
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
}

impl ParallelDecoder {
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
        }
    }

    /// With custom name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
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

    fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};

        // strobe/dN/cs alias raw binary channels: prefer skip-ahead
        // queries. enable_signal usually comes from a computed control
        // path (an SR latch / logic gate) whose producer only streams,
        // but declares EdgeQuery too so that a graph wiring it straight
        // to a raw channel gets point queries instead of a stream.
        let indexed_protocols = vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream];

        let mut schemas = Vec::new();

        // Block inputs first
        schemas.push(
            PortSchema::new::<SampleBlock>("strobe", 0, PortDirection::Input)
                .with_protocols(indexed_protocols.clone()),
        );

        for i in 0..self.num_data_bits {
            schemas.push(
                PortSchema::new::<SampleBlock>(format!("d{}", i), 1 + i, PortDirection::Input)
                    .with_protocols(indexed_protocols.clone()),
            );
        }

        schemas.push(
            PortSchema::new::<SampleBlock>("cs", 1 + self.num_data_bits, PortDirection::Input)
                .with_protocols(indexed_protocols.clone()),
        );

        // Edge input last
        schemas.push(
            PortSchema::new::<Sample>(
                "enable_signal",
                1 + self.num_data_bits + 1,
                PortDirection::Input,
            )
            .with_protocols(indexed_protocols),
        );

        schemas
    }

    fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};

        vec![PortSchema::new::<Word>("words", 0, PortDirection::Output)]
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
        // EdgeQuery methods return crate::Result, not WorkResult.
        let query_err = |e: crate::Error| WorkError::NodeError(e.to_string());

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
            let cs_inactive = match cs_polarity {
                CsPolarity::ActiveLow => buffers.cs_values[trigger_index],
                CsPolarity::ActiveHigh => !buffers.cs_values[trigger_index],
                CsPolarity::Disabled => true,
            };
            if !cs_inactive {
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

            if let Some(output) = &output {
                output.send(Word::spanning(
                    assembly.value,
                    assembly.first_ts,
                    timestamp_ns.saturating_sub(assembly.first_ts),
                ))?;
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
        let result = self.work_streamed_inner(inputs, outputs, &mut blocks);
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
        let mut next_enable_change_position = self.next_enable_change_position;
        let mut enable_input = if enable_query.is_some() {
            None
        } else {
            inputs
                .get(enable_port_idx)
                .and_then(|port| port.get::<Sample>(&mut self.enable_buffer))
        };
        if enable_query.is_none() && enable_input.is_none() {
            current_enable_value = true;
            next_enable_change_position = u64::MAX;
        }

        // Acquire the next aligned block set only after the previous set is
        // completely consumed. SampleBlock's Arc-backed payload stays in
        // this state while bounded windows advance through it, so no packed
        // bytes are copied or split for scheduler fairness.
        if blocks.strobe.is_none() {
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

            let mut data = Vec::with_capacity(self.num_data_bits);
            for (i, input) in data_inputs.iter_mut().enumerate() {
                let block = input.recv()?;
                aligned(&block, &format!("Data {i}"))?;
                data.push(block);
            }
            let cs = match &mut cs_input {
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
        }

        let strobe_block = blocks.strobe.as_ref().expect("block set acquired above");
        let data_blocks = &blocks.data;
        let cs_block = &blocks.cs;

        let mode = self.mode;
        let cs_polarity = self.cs_polarity;
        let cycles_per_word = self.cycles_per_word;
        let endianness = self.endianness;
        let num_data_bits = self.num_data_bits;
        let num_samples = strobe_block.num_samples;
        let start_pos = strobe_block.start_position;
        let timestamp_step = strobe_block.timestamp_step;
        let window_start = blocks.offset;
        let window_end = window_start
            .saturating_add(Self::STREAM_SAMPLES_PER_CALL)
            .min(num_samples);
        let mut words_emitted = 0u64;
        let mut last_strobe_value = self.last_strobe_value;
        let mut assembly_value = self.assembly_value;
        let mut assembly_cycles = self.assembly_cycles;
        let mut assembly_first_ts = self.assembly_first_ts;

        // Find all triggers directly in packed 64-bit strobe words. The
        // trigger mask is then walked with trailing_zeros, so sparse live
        // signals do work proportional to their edges instead of samples.
        let first_word = window_start / u64::BITS as usize;
        let last_word = (window_end - 1) / u64::BITS as usize;
        for word_index in first_word..=last_word {
            let word_start = word_index * u64::BITS as usize;
            let word = packed_word(&strobe_block.data, word_index);
            let previous_bit = if word_start == 0 {
                last_strobe_value
            } else {
                packed_bit(&strobe_block.data, word_start - 1)
            };
            let toggles = word ^ ((word << 1) | u64::from(previous_bit));
            let mut triggers = match mode {
                StrobeMode::RisingEdge => toggles & word,
                StrobeMode::FallingEdge => toggles & !word,
                StrobeMode::AnyEdge => toggles,
                StrobeMode::HighLevel => word,
                StrobeMode::LowLevel => !word,
            };
            let range_start = window_start.saturating_sub(word_start);
            let range_end = window_end
                .saturating_sub(word_start)
                .min(u64::BITS as usize);
            triggers &= bit_range_mask(range_start, range_end);

            while triggers != 0 {
                let bit_in_word = triggers.trailing_zeros() as usize;
                triggers &= triggers - 1;
                let local_idx = word_start + bit_in_word;
                let position = start_pos + local_idx as u64;

                // Check CS: is it inactive?
                let cs_inactive = match (cs_polarity, cs_block) {
                    (CsPolarity::ActiveLow, Some(cs)) => packed_bit(&cs.data, local_idx), // inactive = high
                    (CsPolarity::ActiveHigh, Some(cs)) => !packed_bit(&cs.data, local_idx), // inactive = low
                    _ => true, // Disabled or unconnected
                };

                if !cs_inactive {
                    // A gated-off cycle breaks any word being assembled.
                    if assembly_cycles > 0 {
                        debug!(
                            "[{}] dropping incomplete word ({}/{} cycles) at CS boundary",
                            self.name, assembly_cycles, cycles_per_word
                        );
                        assembly_cycles = 0;
                        assembly_value = 0;
                    }
                    continue;
                }

                // Check enable signal: point-read when EdgeQuery-negotiated,
                // else advance the streamed edge level (inline advance logic).
                let timestamp_ns = position.saturating_mul(timestamp_step);
                if let Some(q) = &enable_query {
                    current_enable_value = q
                        .value_at(position)
                        .map_err(|e| WorkError::NodeError(e.to_string()))?;
                } else if timestamp_ns >= next_enable_change_position
                    && let Some(enable) = &mut enable_input
                {
                    loop {
                        match enable.peek() {
                            Ok(next_edge) => {
                                if next_edge.start_time_ns <= timestamp_ns {
                                    let edge = enable.recv()?;
                                    current_enable_value = edge.value;
                                } else {
                                    next_enable_change_position = next_edge.start_time_ns;
                                    break;
                                }
                            }
                            Err(WorkError::Shutdown) => {
                                next_enable_change_position = u64::MAX;
                                break;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                }

                if !current_enable_value {
                    // A gated-off cycle breaks any word being assembled.
                    if assembly_cycles > 0 {
                        debug!(
                            "[{}] dropping incomplete word ({}/{} cycles) at enable boundary",
                            self.name, assembly_cycles, cycles_per_word
                        );
                        assembly_cycles = 0;
                        assembly_value = 0;
                    }
                    continue;
                }

                // Sample all data bits only at selected trigger positions.
                let mut value = 0u64;
                for (bit_idx, db) in data_blocks.iter().enumerate() {
                    if packed_bit(&db.data, local_idx) {
                        value |= 1 << bit_idx;
                    }
                }

                // Assemble cycles into a word (cycles_per_word == 1 passes
                // each cycle straight through).
                if assembly_cycles == 0 {
                    assembly_first_ts = timestamp_ns;
                }
                match endianness {
                    Endianness::Little => {
                        assembly_value |= value << (assembly_cycles * num_data_bits);
                    }
                    Endianness::Big => {
                        assembly_value = (assembly_value << num_data_bits) | value;
                    }
                }
                assembly_cycles += 1;
                if assembly_cycles < cycles_per_word {
                    continue;
                }

                // First to last assembled cycle; a single-cycle word is an
                // instant (its value stands on the bus until the next strobe).
                let word = Word::spanning(
                    assembly_value,
                    assembly_first_ts,
                    timestamp_ns.saturating_sub(assembly_first_ts),
                );
                assembly_value = 0;
                assembly_cycles = 0;

                if let Some(output) = &output {
                    output.send(word)?;
                }
                words_emitted += 1;
            }
        }

        last_strobe_value = packed_bit(&strobe_block.data, window_end - 1);

        // Save state back
        self.last_strobe_value = last_strobe_value;
        self.current_enable_value = current_enable_value;
        self.next_enable_change_position = next_enable_change_position;
        self.total_words_emitted += words_emitted;
        self.assembly_value = assembly_value;
        self.assembly_cycles = assembly_cycles;
        self.assembly_first_ts = assembly_first_ts;

        blocks.offset = window_end;
        if window_end == num_samples {
            blocks.strobe = None;
            blocks.data.clear();
            blocks.cs = None;
            blocks.offset = 0;
        }

        if self.work_call_count.is_multiple_of(10) || words_emitted > 0 {
            debug!(
                "[{}] Stream window {} done: {} words this window, {} total",
                self.name, self.work_call_count, words_emitted, self.total_words_emitted
            );
        }

        Ok(words_emitted as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::node::ProcessNode;
    use crate::runtime::sender::{ChannelMessage, Sender};
    use crate::runtime::watchdog::Watchdog;
    use crossbeam_channel::bounded;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_decoder_creation() {
        let decoder = ParallelDecoder::new(8, StrobeMode::RisingEdge, CsPolarity::ActiveLow);
        assert_eq!(decoder.num_data_bits, 8);
        assert_eq!(decoder.cs_polarity, CsPolarity::ActiveLow);
        // Block inputs: strobe + 8 data + cs = 10, Edge input: enable = 1, Total = 11
        assert_eq!(decoder.num_inputs(), 11);
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
        InputPort::from_type_erased(Box::new(()) as Box<dyn std::any::Any + Send>).with_watchdog(
            wd.clone(),
            "pd".to_string(),
            name.to_string(),
        )
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
        out_rx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(w) => Some(w),
                _ => None,
            })
            .collect()
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
        out_rx
            .try_iter()
            .filter_map(|message| match message {
                ChannelMessage::Sample(word) => Some(word),
                _ => None,
            })
            .collect()
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

    #[test]
    fn streamed_block_is_retained_and_processed_in_bounded_windows() {
        let wd = Watchdog::new();
        let sample_count = 2 * ParallelDecoder::STREAM_SAMPLES_PER_CALL + 10;
        let backing: Arc<[u8]> = Arc::from(vec![0u8; sample_count.div_ceil(8)].into_boxed_slice());
        let strobe = SampleBlock::new(backing.clone(), 0, sample_count, 1);
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
        let mut decoder = ParallelDecoder::new(1, StrobeMode::AnyEdge, CsPolarity::Disabled);

        assert_eq!(decoder.work(&inputs, &outputs).unwrap(), 0);
        let resident = decoder.stream_blocks.strobe.as_ref().unwrap();
        assert!(Arc::ptr_eq(&backing, &resident.data));
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
        fn value_at(&self, position: u64) -> crate::Result<bool> {
            Ok(self.bits[position as usize])
        }
        fn next_edge(
            &self,
            position: u64,
            limit: u64,
        ) -> crate::Result<Option<crate::runtime::capture::CaptureTransition>> {
            let mut current = self.bits[position as usize];
            for p in (position + 1)..limit.min(self.total_samples()) {
                let value = self.bits[p as usize];
                if value != current {
                    return Ok(Some(crate::runtime::capture::CaptureTransition {
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
        fn value_at(&self, position: u64) -> crate::Result<bool> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.bits[position as usize])
        }
        fn next_edge(
            &self,
            _position: u64,
            _limit: u64,
        ) -> crate::Result<Option<CaptureTransition>> {
            unreachable!("data-only test query must not be searched for edges")
        }
    }

    fn query_input(wd: &Watchdog, bits: &[bool], name: &str) -> InputPort {
        InputPort::from_type_erased(Box::new(()) as Box<dyn std::any::Any + Send>)
            .with_edge_query(Some(Arc::new(FakeChannel {
                bits: bits.to_vec(),
            })))
            .with_watchdog(wd.clone(), "pd".to_string(), name.to_string())
    }

    #[test]
    fn query_mode_does_not_read_data_at_gated_triggers() {
        let wd = Watchdog::new();
        let reads = Arc::new(AtomicUsize::new(0));
        let data_query: Arc<dyn EdgeQuery> = Arc::new(CountingChannel {
            bits: vec![true; 4],
            reads: reads.clone(),
        });
        let data_input = InputPort::from_type_erased(Box::new(()) as Box<dyn std::any::Any + Send>)
            .with_edge_query(Some(data_query))
            .with_watchdog(wd.clone(), "pd".to_string(), "d0".to_string());
        let inputs = [
            query_input(&wd, &[false, true, false, true], "strobe"),
            data_input,
            // Active-low CS remains low, gating every rising strobe.
            query_input(&wd, &[false; 4], "cs"),
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
        let words: Vec<_> = out_rx
            .try_iter()
            .filter_map(|message| match message {
                ChannelMessage::Sample(word) => Some(word),
                _ => None,
            })
            .collect();
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
        out_rx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(w) => Some(w),
                _ => None,
            })
            .collect()
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

    /// Wraps a node and forces its outputs onto the `Stream` protocol
    /// regardless of what the wrapped node would otherwise prefer.
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
        fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
            self.0.input_schema()
        }
        fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
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
    }

    /// Test-only sink that collects everything sent to its single input.
    struct CollectWords(Arc<std::sync::Mutex<Vec<Word>>>);

    impl ProcessNode for CollectWords {
        fn name(&self) -> &str {
            "collect"
        }
        fn num_inputs(&self) -> usize {
            1
        }
        fn num_outputs(&self) -> usize {
            0
        }
        fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
            use crate::runtime::ports::{PortDirection, PortSchema};
            vec![PortSchema::new::<Word>("data", 0, PortDirection::Input)]
        }
        fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            let mut buf = VecDeque::new();
            let mut recv = inputs
                .first()
                .and_then(|p| p.get::<Word>(&mut buf))
                .ok_or_else(|| WorkError::NodeError("Missing collector input".into()))?;
            let item = recv.recv()?;
            self.0.lock().unwrap().push(item);
            Ok(1)
        }
    }

    /// Runs `_captures/wipneus5.dsl` through a real 3-node pipeline
    /// (`DslFileSource` -> `ParallelDecoder` -> collector), bounded to
    /// `max_samples` so the test is fast, and returns the decoded words.
    /// Channels 0..7 = data, 8 = CS, 10 = strobe — the same mapping the
    /// UI compiler's golden SPI/parallel pipeline uses on this fixture.
    /// `enable_signal` is left unconnected (always enabled) to keep the
    /// test focused on the strobe/data/CS query-mode path this change
    /// touches. `force_stream` wraps the source so the connection
    /// negotiates `Stream` instead of the `EdgeQuery` both sides would
    /// otherwise prefer.
    fn decode_wipneus5_parallel(
        path: &std::path::Path,
        max_samples: u64,
        force_stream: bool,
    ) -> Vec<Word> {
        use crate::DslFileSource;
        use crate::runtime::Pipeline;

        let source = DslFileSource::new(path, 11)
            .expect("wipneus5.dsl should open")
            .with_max_samples(Some(max_samples));
        let decoder = ParallelDecoder::new(8, StrobeMode::AnyEdge, CsPolarity::ActiveLow);
        let collected = Arc::new(std::sync::Mutex::new(Vec::new()));

        let mut pipeline = Pipeline::new();
        if force_stream {
            pipeline
                .add_process("source", ForceStreamOutput(source))
                .unwrap();
        } else {
            pipeline.add_process("source", source).unwrap();
        }
        pipeline.add_process("decoder", decoder).unwrap();
        pipeline
            .add_process("collect", CollectWords(collected.clone()))
            .unwrap();

        pipeline
            .connect("source", "ch10", "decoder", "strobe")
            .unwrap();
        for bit in 0..8 {
            pipeline
                .connect("source", &format!("ch{bit}"), "decoder", &format!("d{bit}"))
                .unwrap();
        }
        pipeline.connect("source", "ch8", "decoder", "cs").unwrap();
        pipeline
            .connect("decoder", "words", "collect", "data")
            .unwrap();

        pipeline.build().unwrap().wait();

        Arc::try_unwrap(collected).unwrap().into_inner().unwrap()
    }

    #[test]
    fn test_query_mode_matches_streaming_mode() {
        let path = std::path::Path::new("_captures/wipneus5.dsl");
        if !path.exists() {
            return;
        }

        // Bounded prefix: fast to run, still large enough to very likely
        // contain real bus activity on this fixture.
        const MAX_SAMPLES: u64 = 200_000_000;

        let streamed = decode_wipneus5_parallel(path, MAX_SAMPLES, true);
        let queried = decode_wipneus5_parallel(path, MAX_SAMPLES, false);

        assert!(
            !streamed.is_empty(),
            "expected at least one decoded word in the first {MAX_SAMPLES} samples \
             to make this comparison meaningful"
        );

        let as_tuple = |w: &Word| (w.value, w.timestamp_ns, w.duration_ns);
        let streamed_view: Vec<_> = streamed.iter().map(as_tuple).collect();
        let queried_view: Vec<_> = queried.iter().map(as_tuple).collect();

        assert_eq!(
            streamed_view, queried_view,
            "query-mode ParallelDecoder must produce byte-identical output to the streaming path"
        );
    }
}
