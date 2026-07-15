use std::sync::Arc;

use crate::events::Word;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreStatus {
    Live,
    Finished,
    Cancelled,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveStoreMetadata {
    pub generation: u64,
    pub committed_block_count: usize,
    pub committed_word_count: u64,
    pub committed_data_len: u64,
    pub first_timestamp_ns: Option<u64>,
    pub last_timestamp_ns: Option<u64>,
    pub extent_end_ns: Option<u64>,
    pub hot_tail_word_count: usize,
    pub mmap_backed: bool,
    pub status: StoreStatus,
}

#[derive(Debug, Clone)]
pub struct LiveStoreSnapshot {
    pub metadata: LiveStoreMetadata,
    pub hot_tail: Arc<[Word]>,
}
