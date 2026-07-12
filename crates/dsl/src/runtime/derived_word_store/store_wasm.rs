//! In-memory derived-word store for wasm.
//!
//! This implements the same store/query contracts as the native file-backed
//! implementation. Persistent-cache configuration is accepted as metadata but
//! deliberately has no filesystem effect.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use super::CodecError;
use super::config::{LiveStoreConfig, PersistentStoreConfig};
use super::query::{
    AnnotationQuery, AnnotationQueryError, AnnotationQueryResult, AnnotationStoreMetadata,
    ExactAnnotationWindow, WordPresenceBucket,
};
use super::state::{LiveStoreMetadata, LiveStoreSnapshot, StoreStatus};
use crate::runtime::{Annotation, Word};

pub(super) fn default_working_directory() -> PathBuf {
    PathBuf::new()
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("derived-word store codec error: {0}")]
    Codec(#[from] CodecError),

    #[error("derived-word store is not live: {0:?}")]
    NotLive(StoreStatus),

    #[error("persistent derived-word storage is unavailable on wasm")]
    PersistenceUnsupported,
}

pub type StoreResult<T> = std::result::Result<T, StoreError>;

struct MemoryState {
    words: Vec<Word>,
    generation: u64,
    status: StoreStatus,
}

#[derive(Clone)]
pub struct IndexedAnnotationStore {
    state: Arc<RwLock<MemoryState>>,
}

impl IndexedAnnotationStore {
    pub fn open_persistent(
        _config: &PersistentStoreConfig,
    ) -> StoreResult<Option<IndexedAnnotationStore>> {
        Ok(None)
    }

    pub fn snapshot(&self) -> LiveStoreSnapshot {
        let state = self.state.read().unwrap();
        LiveStoreSnapshot {
            metadata: LiveStoreMetadata {
                generation: state.generation,
                committed_block_count: usize::from(!state.words.is_empty()),
                committed_word_count: state.words.len() as u64,
                committed_data_len: 0,
                first_timestamp_ns: state.words.first().map(|word| word.timestamp_ns),
                last_timestamp_ns: state.words.last().map(|word| word.timestamp_ns),
                extent_end_ns: state
                    .words
                    .iter()
                    .map(|word| word.timestamp_ns.saturating_add(word.duration_ns))
                    .max(),
                hot_tail_word_count: state.words.len(),
                mmap_backed: false,
                status: state.status.clone(),
            },
            hot_tail: Arc::from(state.words.clone()),
        }
    }
}

impl AnnotationQuery for IndexedAnnotationStore {
    fn metadata(&self) -> AnnotationStoreMetadata {
        let snapshot = self.snapshot();
        AnnotationStoreMetadata {
            generation: snapshot.metadata.generation,
            total_word_count: snapshot.metadata.committed_word_count,
            first_timestamp_ns: snapshot.metadata.first_timestamp_ns,
            last_timestamp_ns: snapshot.metadata.last_timestamp_ns,
            extent_end_ns: snapshot.metadata.extent_end_ns,
        }
    }

    fn presence_window(
        &self,
        start_ns: u64,
        end_ns: u64,
        target_buckets: usize,
    ) -> AnnotationQueryResult<Vec<WordPresenceBucket>> {
        if start_ns > end_ns {
            return Err(AnnotationQueryError::InvalidWindow { start_ns, end_ns });
        }
        if target_buckets == 0 {
            return Err(AnnotationQueryError::ZeroBucketLimit);
        }
        let exact = self.exact_window(start_ns, end_ns, usize::MAX)?;
        if exact.annotations.is_empty() {
            return Ok(Vec::new());
        }
        let span = end_ns.saturating_sub(start_ns).saturating_add(1);
        let width = span.div_ceil(target_buckets as u64).max(1);
        let mut buckets: Vec<WordPresenceBucket> = Vec::new();
        for annotation in exact.annotations {
            let bucket_start = start_ns
                .saturating_add(annotation.start_ns.saturating_sub(start_ns) / width * width);
            if let Some(bucket) = buckets.last_mut()
                && bucket.start_ns == bucket_start
            {
                bucket.word_count += 1;
                bucket.end_ns = bucket.end_ns.max(annotation.end_ns);
            } else {
                buckets.push(WordPresenceBucket {
                    start_ns: bucket_start,
                    end_ns: annotation.end_ns.max(bucket_start.saturating_add(width)),
                    word_count: 1,
                });
            }
        }
        Ok(buckets)
    }

    fn exact_window(
        &self,
        start_ns: u64,
        end_ns: u64,
        max_words: usize,
    ) -> AnnotationQueryResult<ExactAnnotationWindow> {
        if start_ns > end_ns {
            return Err(AnnotationQueryError::InvalidWindow { start_ns, end_ns });
        }
        if max_words == 0 {
            return Err(AnnotationQueryError::ZeroWordLimit);
        }
        let state = self.state.read().unwrap();
        let mut annotations = Vec::new();
        for (index, word) in state.words.iter().enumerate() {
            let annotation_end = if word.duration_ns > 0 {
                word.timestamp_ns.saturating_add(word.duration_ns)
            } else {
                state
                    .words
                    .get(index + 1)
                    .map_or(word.timestamp_ns, |next| next.timestamp_ns)
            };
            if word.timestamp_ns <= end_ns && annotation_end >= start_ns {
                if annotations.len() == max_words {
                    return Ok(ExactAnnotationWindow {
                        annotations,
                        complete: false,
                        generation: state.generation,
                    });
                }
                annotations.push(Annotation {
                    start_ns: word.timestamp_ns,
                    end_ns: annotation_end,
                    value: word.value,
                });
            }
        }
        Ok(ExactAnnotationWindow {
            annotations,
            complete: true,
            generation: state.generation,
        })
    }

    fn nearest_boundary(
        &self,
        timestamp_ns: u64,
        max_distance_ns: u64,
    ) -> AnnotationQueryResult<Option<u64>> {
        let state = self.state.read().unwrap();
        Ok(state
            .words
            .iter()
            .flat_map(|word| {
                [
                    word.timestamp_ns,
                    word.timestamp_ns.saturating_add(word.duration_ns),
                ]
            })
            .filter(|candidate| candidate.abs_diff(timestamp_ns) <= max_distance_ns)
            .min_by_key(|candidate| candidate.abs_diff(timestamp_ns)))
    }
}

pub struct IndexedAnnotationWriter {
    store: IndexedAnnotationStore,
}

impl IndexedAnnotationWriter {
    pub fn create(config: LiveStoreConfig) -> StoreResult<(Self, IndexedAnnotationStore)> {
        if config.hot_tail_publish_words == 0 {
            return Err(StoreError::Codec(CodecError::InvalidConfiguration(
                "hot_tail_publish_words must be greater than zero",
            )));
        }
        let store = IndexedAnnotationStore {
            state: Arc::new(RwLock::new(MemoryState {
                words: Vec::new(),
                generation: 0,
                status: StoreStatus::Live,
            })),
        };
        Ok((
            Self {
                store: store.clone(),
            },
            store,
        ))
    }

    pub fn store(&self) -> IndexedAnnotationStore {
        self.store.clone()
    }

    pub fn append(&mut self, word: Word) -> StoreResult<()> {
        self.append_batch(std::slice::from_ref(&word))
    }

    pub fn append_batch(&mut self, words: &[Word]) -> StoreResult<()> {
        let mut state = self.store.state.write().unwrap();
        ensure_live(&state)?;
        if let (Some(previous), Some(first)) = (state.words.last(), words.first())
            && first.timestamp_ns < previous.timestamp_ns
        {
            return Err(StoreError::Codec(CodecError::OutOfOrder {
                index: state.words.len(),
                previous_timestamp_ns: previous.timestamp_ns,
                timestamp_ns: first.timestamp_ns,
            }));
        }
        for pair in words.windows(2) {
            if pair[1].timestamp_ns < pair[0].timestamp_ns {
                return Err(StoreError::Codec(CodecError::OutOfOrder {
                    index: state.words.len(),
                    previous_timestamp_ns: pair[0].timestamp_ns,
                    timestamp_ns: pair[1].timestamp_ns,
                }));
            }
        }
        state.words.extend_from_slice(words);
        state.generation += 1;
        Ok(())
    }

    pub fn publish_hot_tail(&mut self) -> StoreResult<()> {
        ensure_live(&self.store.state.read().unwrap())
    }

    pub fn finish(&mut self) -> StoreResult<()> {
        let mut state = self.store.state.write().unwrap();
        ensure_live(&state)?;
        state.status = StoreStatus::Finished;
        state.generation += 1;
        Ok(())
    }

    pub fn cancel(&mut self) -> StoreResult<()> {
        let mut state = self.store.state.write().unwrap();
        ensure_live(&state)?;
        state.words.clear();
        state.status = StoreStatus::Cancelled;
        state.generation += 1;
        Ok(())
    }
}

fn ensure_live(state: &MemoryState) -> StoreResult<()> {
    if state.status == StoreStatus::Live {
        Ok(())
    } else {
        Err(StoreError::NotLive(state.status.clone()))
    }
}

impl super::backend::AnnotationStoreBackend for IndexedAnnotationStore {
    fn snapshot(&self) -> LiveStoreSnapshot {
        IndexedAnnotationStore::snapshot(self)
    }
}

impl super::backend::AnnotationStoreWriterBackend for IndexedAnnotationWriter {
    fn append_batch(&mut self, words: &[Word]) -> StoreResult<()> {
        IndexedAnnotationWriter::append_batch(self, words)
    }

    fn finish(&mut self) -> StoreResult<()> {
        IndexedAnnotationWriter::finish(self)
    }
}
