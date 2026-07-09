//! Parallel bus decoder for block-based processing
//!
//! Accepts SampleBlock inputs for high-bandwidth signals (strobe, data, CS)
//! and Sample inputs for low-bandwidth control signals (enable_signal).
//! Outputs Word events.

use super::types::{CsPolarity, Endianness, StrobeMode};
use crate::runtime::events::Word;
use crate::runtime::Receiver;
use crate::runtime::WorkError;
use crate::runtime::edge_query::EdgeQuery;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkResult};
use crate::runtime::protocol::ProtocolKind;
use crate::runtime::sample::{Sample, SampleBlock};
use crate::runtime::sender::Sender;
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
        // queries. enable_signal (pushed separately below, keeping the
        // default `[Stream]`) is a computed control signal (from an SR
        // latch, not a raw channel) with no EdgeQuery producer yet, so it
        // always streams.
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
        schemas.push(PortSchema::new::<Sample>(
            "enable_signal",
            1 + self.num_data_bits + 1,
            PortDirection::Input,
        ));

        schemas
    }

    fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};

        vec![PortSchema::new::<Word>(
            "words",
            0,
            PortDirection::Output,
        )]
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
            return self.work_indexed(
                inputs,
                outputs,
                strobe_query.expect("checked above"),
                data_queries
                    .into_iter()
                    .map(|q| q.expect("checked above"))
                    .collect(),
                cs_query,
            );
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

/// Streamed enable-signal state, threaded through `process_trigger` calls
/// (query mode has no EdgeQuery producer for this port yet — see
/// `ParallelDecoder::input_protocols`).
struct EnableState<'a> {
    current: bool,
    next_change_position: u64,
    input: Option<Receiver<'a, Sample>>,
}

impl ParallelDecoder {
    /// Index-driven path: strobe triggers are located by direct skip-ahead
    /// queries instead of a per-sample block scan; data/CS are point-read
    /// at each trigger. `enable_signal` still streams (no query producer
    /// exists for it). One call processes every remaining trigger in the
    /// file — there's no natural per-call window the way SpiDecoder has
    /// CS transactions — so a call can run long on a pathologically dense
    /// `AnyEdge` signal; that's the same signal shape query mode doesn't
    /// help with in the first place (see `work()`'s `sparse_mode` gate).
    #[allow(clippy::too_many_arguments)]
    fn work_indexed(
        &mut self,
        inputs: &[InputPort],
        outputs: &[OutputPort],
        strobe_query: Arc<dyn EdgeQuery>,
        data_queries: Vec<Arc<dyn EdgeQuery>>,
        cs_query: Option<Arc<dyn EdgeQuery>>,
    ) -> WorkResult<usize> {
        self.work_call_count += 1;

        let output = outputs
            .first()
            .and_then(|port| port.get::<Word>())
            .ok_or_else(|| WorkError::NodeError("Missing output".to_string()))?;

        let enable_port_idx = 1 + self.num_data_bits + 1;
        let enable_recv = inputs
            .get(enable_port_idx)
            .and_then(|port| port.get::<Sample>(&mut self.enable_buffer));
        let mut enable = EnableState {
            current: self.current_enable_value,
            next_change_position: self.next_enable_change_position,
            input: enable_recv,
        };
        if enable.input.is_none() {
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

        let mut words_emitted = 0u64;
        let mut position = self.query_strobe_position;
        let exhausted;

        // Mirrors work_streamed's `last_strobe_value` starting `false`: a
        // strobe already at the triggering level at position 0 counts as a
        // trigger there too (there's no real "edge" at the very first
        // sample — the implicit prior sample is treated as low, exactly
        // like the streaming per-sample scan does).
        if position == 0 {
            let value = strobe_query.value_at(0).map_err(query_err)?;
            let triggered0 = match mode {
                StrobeMode::RisingEdge | StrobeMode::AnyEdge => value,
                StrobeMode::FallingEdge => false,
                StrobeMode::HighLevel | StrobeMode::LowLevel => {
                    unreachable!("excluded from query mode by work()'s sparse_mode gate")
                }
            };
            if triggered0 {
                words_emitted += Self::process_trigger(
                    0,
                    cs_polarity,
                    &cs_query,
                    &mut enable,
                    &data_queries,
                    num_data_bits,
                    cycles_per_word,
                    endianness,
                    timestamp_step,
                    &mut assembly,
                    &output,
                    &self.name,
                )?;
            }
        }

        loop {
            let next = match mode {
                StrobeMode::RisingEdge => {
                    strobe_query.next_edge_with_value(position, true, total_samples)
                }
                StrobeMode::FallingEdge => {
                    strobe_query.next_edge_with_value(position, false, total_samples)
                }
                StrobeMode::AnyEdge => strobe_query.next_edge(position, total_samples),
                StrobeMode::HighLevel | StrobeMode::LowLevel => {
                    unreachable!("excluded from query mode by work()'s sparse_mode gate")
                }
            }
            .map_err(query_err)?;

            let Some(edge) = next else {
                exhausted = true;
                break;
            };
            position = edge.sample;

            words_emitted += Self::process_trigger(
                position,
                cs_polarity,
                &cs_query,
                &mut enable,
                &data_queries,
                num_data_bits,
                cycles_per_word,
                endianness,
                timestamp_step,
                &mut assembly,
                &output,
                &self.name,
            )?;
        }

        self.query_strobe_position = position;
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

    /// Gates CS/enable, samples data bits, and assembles/emits a word for
    /// one located trigger position. Shared by the position-0 special case
    /// and the main search loop in `work_indexed`.
    #[allow(clippy::too_many_arguments)]
    fn process_trigger(
        position: u64,
        cs_polarity: CsPolarity,
        cs_query: &Option<Arc<dyn EdgeQuery>>,
        enable: &mut EnableState,
        data_queries: &[Arc<dyn EdgeQuery>],
        num_data_bits: usize,
        cycles_per_word: usize,
        endianness: Endianness,
        timestamp_step: u64,
        assembly: &mut AssemblyState,
        output: &Sender<Word>,
        decoder_name: &str,
    ) -> WorkResult<u64> {
        let query_err = |e: crate::Error| WorkError::NodeError(e.to_string());

        // Check CS: is it inactive?
        let cs_inactive = match (cs_polarity, cs_query) {
            (CsPolarity::ActiveLow, Some(q)) => q.value_at(position).map_err(query_err)?, // inactive = high
            (CsPolarity::ActiveHigh, Some(q)) => !q.value_at(position).map_err(query_err)?, // inactive = low
            _ => true, // Disabled or unconnected
        };
        if !cs_inactive {
            if assembly.cycles > 0 {
                debug!(
                    "[{decoder_name}] dropping incomplete word ({}/{} cycles) at CS boundary",
                    assembly.cycles, cycles_per_word
                );
                assembly.cycles = 0;
                assembly.value = 0;
            }
            return Ok(0);
        }

        // Check enable signal (edge-based, timestamps in nanoseconds).
        let timestamp_ns = position.saturating_mul(timestamp_step);
        if timestamp_ns >= enable.next_change_position
            && let Some(enable_recv) = &mut enable.input
        {
            loop {
                match enable_recv.peek() {
                    Ok(next_edge) => {
                        if next_edge.start_time_ns <= timestamp_ns {
                            let edge = enable_recv.recv()?;
                            enable.current = edge.value;
                        } else {
                            enable.next_change_position = next_edge.start_time_ns;
                            break;
                        }
                    }
                    Err(WorkError::Shutdown) => {
                        enable.next_change_position = u64::MAX;
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        if !enable.current {
            if assembly.cycles > 0 {
                debug!(
                    "[{decoder_name}] dropping incomplete word ({}/{} cycles) at enable boundary",
                    assembly.cycles, cycles_per_word
                );
                assembly.cycles = 0;
                assembly.value = 0;
            }
            return Ok(0);
        }

        // Sample all data bits — O(1) (or O(log gap)) point reads.
        let mut value = 0u64;
        for (bit_idx, q) in data_queries.iter().enumerate() {
            if q.value_at(position).map_err(query_err)? {
                value |= 1 << bit_idx;
            }
        }

        // Assemble cycles into a word (cycles_per_word == 1 passes each
        // cycle straight through).
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
            return Ok(0);
        }

        let word = Word {
            value: assembly.value,
            timestamp_ns: assembly.first_ts,
        };
        assembly.value = 0;
        assembly.cycles = 0;

        output.send(word)?;
        Ok(1)
    }

    /// Streaming path: unchanged behavior for live sources or any
    /// connection that didn't negotiate `EdgeQuery`.
    fn work_streamed(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        self.work_call_count += 1;

        if self.work_call_count == 1 {
            debug!(
                "[{}] First work() call: {} inputs, {} outputs",
                self.name,
                inputs.len(),
                outputs.len()
            );
        }

        // Get output
        let output = outputs
            .first()
            .and_then(|port| port.get::<Word>())
            .ok_or_else(|| WorkError::NodeError("Missing output".to_string()))?;

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
        // always enabled.
        // Use local variables for enable state to avoid borrowing self
        let mut current_enable_value = self.current_enable_value;
        let mut next_enable_change_position = self.next_enable_change_position;
        let mut enable_input = inputs
            .get(1 + self.num_data_bits + 1)
            .and_then(|port| port.get::<Sample>(&mut self.enable_buffer));
        if enable_input.is_none() {
            current_enable_value = true;
            next_enable_change_position = u64::MAX;
        }

        // Receive one block from each channel
        let strobe_block = strobe_input.recv()?;
        let mut data_blocks: Vec<SampleBlock> = Vec::with_capacity(self.num_data_bits);
        for (i, input) in data_inputs.iter_mut().enumerate() {
            let block = input.recv()?;
            // Verify alignment
            if block.start_position != strobe_block.start_position {
                return Err(WorkError::NodeError(format!(
                    "Data block {} misaligned: start={} vs strobe start={}",
                    i, block.start_position, strobe_block.start_position
                )));
            }
            data_blocks.push(block);
        }
        let cs_block = match &mut cs_input {
            Some(input) => Some(input.recv()?),
            None => None,
        };

        let mode = self.mode;
        let cs_polarity = self.cs_polarity;
        let cycles_per_word = self.cycles_per_word;
        let endianness = self.endianness;
        let num_data_bits = self.num_data_bits;
        let num_samples = strobe_block.num_samples;
        let start_pos = strobe_block.start_position;
        let timestamp_step = strobe_block.timestamp_step;
        let mut words_emitted = 0u64;
        let mut last_strobe_value = self.last_strobe_value;
        let mut assembly_value = self.assembly_value;
        let mut assembly_cycles = self.assembly_cycles;
        let mut assembly_first_ts = self.assembly_first_ts;

        // Iterate through all samples in this block
        for local_idx in 0..num_samples {
            let position = start_pos + local_idx as u64;
            let strobe_val = strobe_block.get_bit(position);

            // Check strobe trigger
            let triggered = match mode {
                StrobeMode::RisingEdge => !last_strobe_value && strobe_val,
                StrobeMode::FallingEdge => last_strobe_value && !strobe_val,
                StrobeMode::AnyEdge => last_strobe_value != strobe_val,
                StrobeMode::HighLevel => strobe_val,
                StrobeMode::LowLevel => !strobe_val,
            };
            last_strobe_value = strobe_val;

            if !triggered {
                continue;
            }

            // Check CS: is it inactive?
            let cs_inactive = match (cs_polarity, &cs_block) {
                (CsPolarity::ActiveLow, Some(cs)) => cs.get_bit(position), // inactive = high
                (CsPolarity::ActiveHigh, Some(cs)) => !cs.get_bit(position), // inactive = low
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

            // Check enable signal (edge-based, timestamps in nanoseconds) — inline advance logic
            let timestamp_ns = position * timestamp_step;
            if timestamp_ns >= next_enable_change_position
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

            // Sample all data bits — O(1) per bit
            let mut value = 0u64;
            for (bit_idx, db) in data_blocks.iter().enumerate() {
                if db.get_bit(position) {
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

            let word = Word {
                value: assembly_value,
                timestamp_ns: assembly_first_ts,
            };
            assembly_value = 0;
            assembly_cycles = 0;

            output.send(word)?;
            words_emitted += 1;
        }

        // Save state back
        self.last_strobe_value = last_strobe_value;
        self.current_enable_value = current_enable_value;
        self.next_enable_change_position = next_enable_change_position;
        self.total_words_emitted += words_emitted;
        self.assembly_value = assembly_value;
        self.assembly_cycles = assembly_cycles;
        self.assembly_first_ts = assembly_first_ts;

        if self.work_call_count.is_multiple_of(10) || words_emitted > 0 {
            debug!(
                "[{}] Block {} done: {} words this block, {} total",
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

        pipeline.connect("source", "ch10", "decoder", "strobe").unwrap();
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

        let as_tuple = |w: &Word| (w.value, w.timestamp_ns);
        let streamed_view: Vec<_> = streamed.iter().map(as_tuple).collect();
        let queried_view: Vec<_> = queried.iter().map(as_tuple).collect();

        assert_eq!(
            streamed_view, queried_view,
            "query-mode ParallelDecoder must produce byte-identical output to the streaming path"
        );
    }
}
