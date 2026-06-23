//! Parallel bus decoder for block-based processing
//!
//! Accepts SampleBlock inputs for high-bandwidth signals (strobe, data, CS)
//! and Sample inputs for low-bandwidth control signals (enable_signal).
//! Outputs ParallelWord events.

use super::types::{CsPolarity, ParallelWord, StrobeMode, TimingInfo};
use crate::runtime::Receiver;
use crate::runtime::WorkError;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkResult};
use crate::runtime::sample::{Sample, SampleBlock};
use std::collections::VecDeque;
use tracing::debug;

/// Parallel bus decoder node (block-based)
///
/// Inputs:
///   - Block inputs: strobe_block, d0_block..dN_block, cs_block — all SampleBlock
///   - Edge input: enable_signal — Sample (from SPI controller)
///
/// Output: ParallelWord events
pub struct ParallelDecoder {
    name: String,
    num_data_bits: usize,
    mode: StrobeMode,
    cs_polarity: CsPolarity,

    /// Putback buffer for enable_signal (edge-based Sample input)
    enable_buffer: VecDeque<Sample>,

    /// Current enable state from edge-based enable_signal
    current_enable_value: bool,
    /// Position up to which the current enable value is known to be valid
    next_enable_change_position: u64,

    last_strobe_value: bool,
    work_call_count: usize,
    total_words_emitted: u64,
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
            enable_buffer: VecDeque::new(),
            current_enable_value: false,
            next_enable_change_position: 0,
            last_strobe_value: false,
            work_call_count: 0,
            total_words_emitted: 0,
        }
    }

    /// With custom name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
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
        1 // ParallelWord output
    }

    fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};

        let mut schemas = Vec::new();

        // Block inputs first
        schemas.push(PortSchema::new::<SampleBlock>(
            "strobe",
            0,
            PortDirection::Input,
        ));

        for i in 0..self.num_data_bits {
            schemas.push(PortSchema::new::<SampleBlock>(
                format!("d{}", i),
                1 + i,
                PortDirection::Input,
            ));
        }

        schemas.push(PortSchema::new::<SampleBlock>(
            "cs",
            1 + self.num_data_bits,
            PortDirection::Input,
        ));

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

        vec![PortSchema::new::<ParallelWord>(
            "words",
            0,
            PortDirection::Output,
        )]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
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
            .and_then(|port| port.get::<ParallelWord>())
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

        let mut cs_buf = VecDeque::new();
        let mut cs_input = inputs
            .get(1 + self.num_data_bits)
            .and_then(|port| port.get::<SampleBlock>(&mut cs_buf))
            .ok_or_else(|| WorkError::NodeError("Missing cs block input".to_string()))?;

        // Get edge input: enable_signal
        // Use local variables for enable state to avoid borrowing self
        let mut current_enable_value = self.current_enable_value;
        let mut next_enable_change_position = self.next_enable_change_position;
        let mut enable_input = inputs
            .get(1 + self.num_data_bits + 1)
            .and_then(|port| port.get::<Sample>(&mut self.enable_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing enable_signal input".to_string()))?;

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
        let cs_block = cs_input.recv()?;

        let mode = self.mode;
        let cs_polarity = self.cs_polarity;
        let num_samples = strobe_block.num_samples;
        let start_pos = strobe_block.start_position;
        let timestamp_step = strobe_block.timestamp_step;
        let mut words_emitted = 0u64;
        let mut last_strobe_value = self.last_strobe_value;

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
            let cs_inactive = match cs_polarity {
                CsPolarity::ActiveLow => cs_block.get_bit(position), // inactive = high
                CsPolarity::ActiveHigh => !cs_block.get_bit(position), // inactive = low
                CsPolarity::Disabled => true,
            };

            if !cs_inactive {
                continue;
            }

            // Check enable signal (edge-based, timestamps in nanoseconds) — inline advance logic
            let timestamp_ns = position * timestamp_step;
            if timestamp_ns >= next_enable_change_position {
                loop {
                    match enable_input.peek() {
                        Ok(next_edge) => {
                            if next_edge.start_time <= timestamp_ns {
                                let edge = enable_input.recv()?;
                                current_enable_value = edge.value;
                            } else {
                                next_enable_change_position = next_edge.start_time;
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
                continue;
            }

            // Sample all data bits — O(1) per bit
            let mut value = 0u64;
            for (bit_idx, db) in data_blocks.iter().enumerate() {
                if db.get_bit(position) {
                    value |= 1 << bit_idx;
                }
            }

            let word = ParallelWord {
                value,
                timing: TimingInfo::new(
                    timestamp_ns as f64 / 1_000.0, // Convert ns to microseconds
                    timestamp_ns,
                ),
            };

            output.send(word)?;
            words_emitted += 1;
        }

        // Save state back
        self.last_strobe_value = last_strobe_value;
        self.current_enable_value = current_enable_value;
        self.next_enable_change_position = next_enable_change_position;
        self.total_words_emitted += words_emitted;

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

    #[test]
    fn test_decoder_creation() {
        let decoder = ParallelDecoder::new(8, StrobeMode::RisingEdge, CsPolarity::ActiveLow);
        assert_eq!(decoder.num_data_bits, 8);
        assert_eq!(decoder.cs_polarity, CsPolarity::ActiveLow);
        // Block inputs: strobe + 8 data + cs = 10, Edge input: enable = 1, Total = 11
        assert_eq!(decoder.num_inputs(), 11);
    }
}
