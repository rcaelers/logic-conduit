use super::DecodedWordBlock;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub const DEFAULT_DECODED_BLOCK_CACHE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodedBlockCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
    pub memory_bytes: usize,
    pub budget_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    store_id: u64,
    sequence: u64,
}

struct CacheEntry {
    block: Arc<DecodedWordBlock>,
    memory_bytes: usize,
    last_access: u64,
}

struct DecodedBlockCache {
    entries: HashMap<CacheKey, CacheEntry>,
    memory_bytes: usize,
    budget_bytes: usize,
    access_clock: u64,
    hits: u64,
    misses: u64,
}

impl DecodedBlockCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            memory_bytes: 0,
            budget_bytes: DEFAULT_DECODED_BLOCK_CACHE_BYTES,
            access_clock: 0,
            hits: 0,
            misses: 0,
        }
    }

    fn get(&mut self, key: CacheKey) -> Option<Arc<DecodedWordBlock>> {
        self.access_clock = self.access_clock.wrapping_add(1);
        let Some(entry) = self.entries.get_mut(&key) else {
            self.misses += 1;
            return None;
        };
        self.hits += 1;
        entry.last_access = self.access_clock;
        Some(Arc::clone(&entry.block))
    }

    fn insert(&mut self, key: CacheKey, block: Arc<DecodedWordBlock>) {
        let memory_bytes = decoded_block_bytes(&block);
        if memory_bytes > self.budget_bytes {
            return;
        }
        self.access_clock = self.access_clock.wrapping_add(1);
        if let Some(previous) = self.entries.remove(&key) {
            self.memory_bytes -= previous.memory_bytes;
        }
        self.memory_bytes += memory_bytes;
        self.entries.insert(
            key,
            CacheEntry {
                block,
                memory_bytes,
                last_access: self.access_clock,
            },
        );
        self.evict_to_budget();
    }

    fn evict_to_budget(&mut self) {
        while self.memory_bytes > self.budget_bytes {
            let Some((&oldest_key, _)) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
            else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest_key) {
                self.memory_bytes -= removed.memory_bytes;
            }
        }
    }

    fn stats(&self) -> DecodedBlockCacheStats {
        DecodedBlockCacheStats {
            hits: self.hits,
            misses: self.misses,
            entries: self.entries.len(),
            memory_bytes: self.memory_bytes,
            budget_bytes: self.budget_bytes,
        }
    }
}

fn decoded_block_bytes(block: &DecodedWordBlock) -> usize {
    size_of::<DecodedWordBlock>()
        + block.words.capacity() * size_of::<crate::runtime::Word>()
        + block.restarts.capacity() * size_of::<super::RestartEntry>()
}

fn shared_cache() -> &'static Mutex<DecodedBlockCache> {
    static CACHE: OnceLock<Mutex<DecodedBlockCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(DecodedBlockCache::new()))
}

pub(super) fn cached_block(store_id: u64, sequence: u64) -> Option<Arc<DecodedWordBlock>> {
    shared_cache()
        .lock()
        .unwrap()
        .get(CacheKey { store_id, sequence })
}

pub(super) fn cache_block(store_id: u64, block: Arc<DecodedWordBlock>) {
    let sequence = block.header.sequence;
    shared_cache()
        .lock()
        .unwrap()
        .insert(CacheKey { store_id, sequence }, block);
}

pub fn configure_decoded_block_cache(budget_bytes: usize) {
    let mut cache = shared_cache().lock().unwrap();
    cache.budget_bytes = budget_bytes;
    cache.evict_to_budget();
}

pub fn decoded_block_cache_stats() -> DecodedBlockCacheStats {
    shared_cache().lock().unwrap().stats()
}

pub fn reset_decoded_block_cache_stats() {
    let mut cache = shared_cache().lock().unwrap();
    cache.hits = 0;
    cache.misses = 0;
}

#[cfg(test)]
pub(super) fn cache_contains(store_id: u64, sequence: u64) -> bool {
    shared_cache()
        .lock()
        .unwrap()
        .entries
        .contains_key(&CacheKey { store_id, sequence })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Word;
    use crate::runtime::derived_word_store::WordBlockHeader;

    fn block(sequence: u64, words: usize) -> Arc<DecodedWordBlock> {
        Arc::new(DecodedWordBlock {
            header: WordBlockHeader {
                flags: 0,
                sequence,
                first_timestamp_ns: 0,
                last_timestamp_ns: words.saturating_sub(1) as u64,
                word_count: words as u32,
                value_bytes: 1,
                record_payload_len: 0,
                restart_count: 0,
                restart_table_offset: 0,
                duration_count: 0,
                duration_table_offset: 0,
                block_len: 0,
                crc32c: 0,
            },
            restarts: Vec::new(),
            words: (0..words)
                .map(|timestamp| Word::new(0, timestamp as u64))
                .collect(),
        })
    }

    #[test]
    fn byte_budget_evicts_the_least_recently_used_block() {
        let first = block(1, 32);
        let second = block(2, 32);
        let third = block(3, 32);
        let one_block = decoded_block_bytes(&first);
        let mut cache = DecodedBlockCache::new();
        cache.budget_bytes = one_block * 2;
        let key = |sequence| CacheKey {
            store_id: 7,
            sequence,
        };

        cache.insert(key(1), first);
        cache.insert(key(2), second);
        assert!(cache.get(key(1)).is_some());
        cache.insert(key(3), third);

        assert!(cache.get(key(1)).is_some());
        assert!(cache.get(key(2)).is_none());
        assert!(cache.get(key(3)).is_some());
        assert!(cache.memory_bytes <= cache.budget_bytes);
    }
}
