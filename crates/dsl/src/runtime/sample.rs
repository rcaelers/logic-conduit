//! Core data types for signal processing

use std::fmt;
use std::sync::Arc;

/// Sample representing a signal value at a specific time
///
/// This is a run-length encoded representation that sends only when a signal changes,
/// dramatically reducing bandwidth for signals that don't toggle frequently.
///
/// The value remains constant until the next Sample arrives. Duration is determined
/// by the timestamp of the next sample (next.start_time_ns - current.start_time_ns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Channel value at this timestamp
    pub value: bool,
    /// Timestamp in nanoseconds when this value started
    pub start_time_ns: u64,
}

impl Sample {
    /// Create a new sample
    pub fn new(value: bool, start_time_ns: u64) -> Self {
        Self { value, start_time_ns }
    }
}

impl fmt::Display for Sample {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Sample[v={}, t={}]", self.value, self.start_time_ns)
    }
}

/// A block of packed-bit samples from a single channel
///
/// Carries raw packed-bit data directly from DSL file blocks, enabling O(1) bit
/// lookup instead of per-edge channel operations. Each block typically covers
/// ~16M samples (~2MB packed).
///
/// All channels in a DSL file share the same block structure:
/// - Same `samples_per_block` (typically 2^24 = 16,777,216)
/// - Same sample positions (all sampled at the same hardware clock)
/// - Blocks are aligned by block number across channels
///
/// This means downstream consumers can simply `recv()` one block per channel
/// and iterate positions in lockstep — no temporal alignment needed.
///
/// ## Bit Packing Format
///
/// LSB-first within each byte: bit N is at `data[N/8] >> (N%8) & 1`.
/// This matches the DSLogic on-disk format, so blocks can be sent with
/// zero transformation from the ZIP archive.
#[derive(Clone, Debug)]
pub struct SampleBlock {
    /// Packed bit data (LSB-first). Shared via Arc for cheap cloning across broadcast.
    pub data: Arc<[u8]>,
    /// Position of the first sample in this block (0-based, global sample index)
    pub start_position: u64,
    /// Number of valid samples in this block (may be < capacity for the last block)
    pub num_samples: usize,
    /// Nanoseconds per sample (1e9 / sample_rate)
    pub timestamp_step: u64,
}

impl SampleBlock {
    /// Create a new SampleBlock
    pub fn new(
        data: Arc<[u8]>,
        start_position: u64,
        num_samples: usize,
        timestamp_step: u64,
    ) -> Self {
        Self {
            data,
            start_position,
            num_samples,
            timestamp_step,
        }
    }

    /// O(1) bit lookup: get the boolean value at a given position within this block.
    ///
    /// `position` is a global sample index. It must be in
    /// `[self.start_position, self.start_position + self.num_samples)`.
    #[inline]
    pub fn get_bit(&self, position: u64) -> bool {
        let local = (position - self.start_position) as usize;
        let byte_index = local / 8;
        let bit_offset = local % 8;
        (self.data[byte_index] >> bit_offset) & 1 == 1
    }

    /// Convert a global sample position to a nanosecond timestamp
    #[inline]
    pub fn position_to_timestamp(&self, position: u64) -> u64 {
        position * self.timestamp_step
    }

    /// The position one past the last valid sample in this block
    #[inline]
    pub fn end_position(&self) -> u64 {
        self.start_position + self.num_samples as u64
    }
}

impl fmt::Display for SampleBlock {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "SampleBlock[start={}, samples={}, bytes={}]",
            self.start_position,
            self.num_samples,
            self.data.len()
        )
    }
}
