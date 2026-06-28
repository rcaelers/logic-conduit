#[derive(Debug, Clone, Copy)]
pub(super) struct IndexHeader {
    pub source_revision: u64,
    pub total_samples: u64,
    pub total_blocks: u64,
    pub samples_per_block: u64,
    pub samplerate_bits: u64,
    pub total_channels: u32,
    pub roots_per_channel: u32,
    pub dir_offset: u64,
    pub payload_offset: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct RootDirEntry {
    pub first_block: u64,
    pub block_count: u32,
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone)]
pub(super) struct LeafSummary {
    pub valid_samples: u32,
    pub first: bool,
    pub last: bool,
    pub active: bool,
    pub l1_toggle: Vec<u64>,
    pub l1_last: Vec<u64>,
    pub l2_toggle: [u64; L2_WORDS],
    pub l2_last: [u64; L2_WORDS],
    pub l3_toggle: u64,
    pub l3_last: u64,
}

#[derive(Debug, Clone)]
pub(super) struct RootChunk {
    pub channel: usize,
    pub root_index: usize,
    pub first_block: u64,
    pub block_count: u32,
    pub root_toggle: u64,
    pub root_first: u64,
    pub root_last: u64,
    pub leaves: Vec<LeafSummary>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct GroupSummary {
    pub toggle: bool,
    pub last: bool,
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

pub(super) const BLOCKS_PER_ROOT: usize = 1;
pub(super) const L1_GROUP_SAMPLES: u64 = 64;
pub(super) const L2_GROUP_SAMPLES: u64 = 4_096;
pub(super) const L3_GROUP_SAMPLES: u64 = 262_144;
pub(super) const L1_WORDS: usize = 4_096;
pub(super) const L2_WORDS: usize = 64;
pub(super) const MAGIC: &[u8; 8] = b"CAPIDX03";
pub(super) const HEADER_SIZE: u64 = 96;
pub(super) const DIR_ENTRY_SIZE: u64 = 48;

pub(super) fn bit(word: u64, index: usize) -> bool {
    index < 64 && ((word >> index) & 1) != 0
}

pub(super) fn set_bit(word: &mut u64, index: usize) {
    if index < 64 {
        *word |= 1_u64 << index;
    }
}
