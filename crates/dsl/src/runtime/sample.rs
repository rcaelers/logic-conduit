//! Core data types for signal processing

use std::fmt;

use super::capture::BlockData;

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
        Self {
            value,
            start_time_ns,
        }
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
    /// Packed bit data (LSB-first), with shared zero-copy backing and range.
    pub data: BlockData,
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
        data: impl Into<BlockData>,
        start_position: u64,
        num_samples: usize,
        timestamp_step: u64,
    ) -> Self {
        Self {
            data: data.into(),
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

    /// Creates a byte-aligned sample subview without copying packed data.
    /// Non-byte-aligned starts cannot use the existing LSB-at-bit-zero wire
    /// representation and therefore return `None`.
    pub fn sub_block(&self, sample_offset: usize, num_samples: usize) -> Option<Self> {
        let sample_end = sample_offset.checked_add(num_samples)?;
        if !sample_offset.is_multiple_of(8) || sample_end > self.num_samples {
            return None;
        }
        let byte_offset = sample_offset / 8;
        let byte_len = num_samples.div_ceil(8);
        Some(Self {
            data: self.data.slice(byte_offset, byte_len)?,
            start_position: self.start_position.checked_add(sample_offset as u64)?,
            num_samples,
            timestamp_step: self.timestamp_step,
        })
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

#[cfg(test)]
mod tests {
    use super::SampleBlock;

    #[test]
    fn byte_aligned_sub_block_shares_backing_and_positions() {
        let block = SampleBlock::new(vec![0b1010_0101, 0b0011_1100, 0b1111_0000], 100, 24, 20);
        let sub = block.sub_block(8, 12).unwrap();

        assert!(block.data.shares_backing(&sub.data));
        assert_eq!(&*sub.data, &[0b0011_1100, 0b1111_0000]);
        assert_eq!(sub.start_position, 108);
        assert_eq!(sub.num_samples, 12);
        assert_eq!(sub.timestamp_step, 20);
        assert!(block.sub_block(1, 8).is_none());
        assert!(block.sub_block(16, 9).is_none());
    }
}
