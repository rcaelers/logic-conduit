#[derive(Debug, Clone, Copy)]
pub(crate) struct IndexHeader {
    pub source_revision: u64,
    pub total_samples: u64,
    pub total_blocks: u64,
    pub samples_per_block: u64,
    pub samplerate_bits: u64,
    pub total_channels: u32,
    pub blocks_per_channel: u32,
    pub dir_offset: u64,
    pub payload_offset: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RootDirEntry {
    pub offset: u64,
    pub len: u64,
    pub toggle: bool,
    pub first: bool,
    pub last: bool,
    pub l3_toggle: u64,
    pub l3_last: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct BlockIndex {
    pub valid_samples: u32,
    pub first: bool,
    pub last: bool,
    /// `None` when the block is constant (no transitions); `Some` otherwise.
    pub levels: Option<Box<BlockLevels>>,
}

#[derive(Debug, Clone)]
pub(crate) struct BlockLevels {
    pub l1_toggle: [u64; L1_WORDS],
    pub l1_last: [u64; L1_WORDS],
    pub l2_toggle: [u64; L2_WORDS],
    pub l2_last: [u64; L2_WORDS],
    pub l3_toggle: u64,
    pub l3_last: u64,
}

impl BlockLevels {
    pub(crate) fn zeroed() -> Box<Self> {
        // SAFETY: BlockLevels consists entirely of u64 / [u64; N] fields;
        // the all-zero bit pattern is valid for all of them.
        unsafe { Box::new_zeroed().assume_init() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureIndexProgress {
    pub completed_roots: usize,
    pub total_roots: usize,
}

impl CaptureIndexProgress {
    pub fn fraction(self) -> f32 {
        if self.total_roots == 0 {
            1.0
        } else {
            self.completed_roots as f32 / self.total_roots as f32
        }
    }
}

/// Bits used to index within one level group: each group covers 2^LEVEL_POWER children.
const LEVEL_POWER: usize = 6;
pub(crate) const SAMPLES_PER_L1_BIT: u64 = (1_usize << LEVEL_POWER) as u64; // 64
pub(crate) const SAMPLES_PER_L2_BIT: u64 = (1_usize << (LEVEL_POWER * 2)) as u64; // 4 096
pub(crate) const SAMPLES_PER_L3_BIT: u64 = (1_usize << (LEVEL_POWER * 3)) as u64; // 262 144
pub(crate) const L1_WORDS: usize = 1 << (LEVEL_POWER * 2); // 64^3 bits / 64 bits-per-word = 4 096
pub(crate) const L2_WORDS: usize = 1 << LEVEL_POWER; // 64^2 bits / 64 bits-per-word = 64
pub(crate) const MAGIC: &[u8; 8] = b"CAPIDX06";
pub(crate) const HEADER_SIZE: u64 = 96;
pub(crate) const DIR_ENTRY_SIZE: u64 = 40;

pub(crate) fn bit(word: u64, index: usize) -> bool {
    index < 64 && ((word >> index) & 1) != 0
}

pub(crate) fn set_bit(word: &mut u64, index: usize) {
    if index < 64 {
        *word |= 1_u64 << index;
    }
}
