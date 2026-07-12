use super::cache::{cache_block, cached_block};
use super::{
    AnnotationQuery, AnnotationQueryError, AnnotationQueryResult, AnnotationStoreMetadata,
    BlockCodecConfig, BlockDirectoryEntry, CodecError, DATA_HEADER_SIZE, DataFileHeader,
    DecodedWordBlock, ExactAnnotationWindow, PushResult, WordBlockBuilder, WordPresenceBucket,
    WordPresenceIndex, WordSummaryRecord, decode_word_block, decode_word_block_range,
};
use crate::runtime::{Annotation, Word};
use memmap2::{Mmap, MmapOptions};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_HOT_TAIL_PUBLISH_WORDS: usize = 16_384;
pub const DEFAULT_HOT_TAIL_PUBLISH_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct LiveStoreConfig {
    pub directory: PathBuf,
    pub cache_key_prefix: [u8; 16],
    pub block: BlockCodecConfig,
    pub hot_tail_publish_words: usize,
    pub hot_tail_publish_interval: Duration,
}

impl Default for LiveStoreConfig {
    fn default() -> Self {
        Self {
            directory: std::env::temp_dir(),
            cache_key_prefix: [0; 16],
            block: BlockCodecConfig::default(),
            hot_tail_publish_words: DEFAULT_HOT_TAIL_PUBLISH_WORDS,
            hot_tail_publish_interval: DEFAULT_HOT_TAIL_PUBLISH_INTERVAL,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreStatus {
    Live,
    Finished,
    Cancelled,
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("derived-word store I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("derived-word store codec error: {0}")]
    Codec(#[from] CodecError),

    #[error("derived-word store is not live: {0:?}")]
    NotLive(StoreStatus),

    #[error("committed word block {index} is out of bounds (block count {block_count})")]
    BlockOutOfBounds { index: usize, block_count: usize },

    #[error("committed word-block directory does not match encoded block {0}")]
    DirectoryMismatch(u64),
}

pub type StoreResult<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveStoreMetadata {
    pub generation: u64,
    pub committed_block_count: usize,
    pub committed_word_count: u64,
    pub committed_data_len: u64,
    pub first_timestamp_ns: Option<u64>,
    pub last_timestamp_ns: Option<u64>,
    pub hot_tail_word_count: usize,
    pub mmap_backed: bool,
    pub status: StoreStatus,
}

#[derive(Debug, Clone)]
pub struct LiveStoreSnapshot {
    pub metadata: LiveStoreMetadata,
    pub hot_tail: Arc<[Word]>,
}

struct LiveState {
    directory: Vec<BlockDirectoryEntry>,
    presence: WordPresenceIndex,
    generation: u64,
    committed_word_count: u64,
    committed_data_len: u64,
    committed_first_timestamp_ns: Option<u64>,
    committed_last_timestamp_ns: Option<u64>,
    hot_tail: Arc<[Word]>,
    status: StoreStatus,
}

struct StoreShared {
    state: RwLock<LiveState>,
    read_backend: RwLock<ReadBackend>,
    store_id: u64,
    // Kept after the file handles so they close before TempPath removes the file.
    temp_path: tempfile::TempPath,
}

enum ReadBackend {
    File(File),
    Mmap(Mmap),
}

impl ReadBackend {
    fn read_exact_at(&self, buffer: &mut [u8], offset: u64) -> io::Result<()> {
        match self {
            Self::File(file) => read_exact_at(file, buffer, offset),
            Self::Mmap(mmap) => {
                let offset = usize::try_from(offset).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "mmap offset exceeds usize")
                })?;
                let end = offset.checked_add(buffer.len()).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "mmap range overflow")
                })?;
                let source = mmap.get(offset..end).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "committed block lies outside derived-word mmap",
                    )
                })?;
                buffer.copy_from_slice(source);
                Ok(())
            }
        }
    }

    fn is_mmap(&self) -> bool {
        matches!(self, Self::Mmap(_))
    }
}

impl StoreShared {
    fn mark_failed(&self, message: String) {
        let mut state = self.state.write().unwrap();
        if matches!(state.status, StoreStatus::Live | StoreStatus::Finished) {
            state.status = StoreStatus::Failed(message);
            state.hot_tail = Arc::from([]);
            state.generation += 1;
        }
    }
}

/// Cloneable read handle for the committed prefix and current hot-tail snapshot.
#[derive(Clone)]
pub struct IndexedAnnotationStore {
    shared: Arc<StoreShared>,
}

impl IndexedAnnotationStore {
    pub fn snapshot(&self) -> LiveStoreSnapshot {
        let mmap_backed = self.shared.read_backend.read().unwrap().is_mmap();
        let state = self.shared.state.read().unwrap();
        let first_timestamp_ns = state
            .committed_first_timestamp_ns
            .or_else(|| state.hot_tail.first().map(|word| word.timestamp_ns));
        let last_timestamp_ns = state
            .hot_tail
            .last()
            .map(|word| word.timestamp_ns)
            .or(state.committed_last_timestamp_ns);
        LiveStoreSnapshot {
            metadata: LiveStoreMetadata {
                generation: state.generation,
                committed_block_count: state.directory.len(),
                committed_word_count: state.committed_word_count,
                committed_data_len: state.committed_data_len,
                first_timestamp_ns,
                last_timestamp_ns,
                hot_tail_word_count: state.hot_tail.len(),
                mmap_backed,
                status: state.status.clone(),
            },
            hot_tail: Arc::clone(&state.hot_tail),
        }
    }

    pub fn directory(&self) -> Vec<BlockDirectoryEntry> {
        self.shared.state.read().unwrap().directory.clone()
    }

    pub fn temp_path(&self) -> &Path {
        &self.shared.temp_path
    }

    /// Reads and validates one fully committed block. The directory lock is
    /// released before any file access or decoding occurs.
    pub fn read_committed_block(&self, index: usize) -> StoreResult<DecodedWordBlock> {
        let entry = {
            let state = self.shared.state.read().unwrap();
            state
                .directory
                .get(index)
                .copied()
                .ok_or(StoreError::BlockOutOfBounds {
                    index,
                    block_count: state.directory.len(),
                })?
        };

        let result = self.read_cached_entry(entry).map(|block| (*block).clone());
        if let Err(error) = &result {
            self.shared.mark_failed(error.to_string());
        }
        result
    }

    fn read_cached_entry(&self, entry: BlockDirectoryEntry) -> StoreResult<Arc<DecodedWordBlock>> {
        if let Some(block) = cached_block(self.shared.store_id, entry.sequence) {
            return Ok(block);
        }
        let bytes = self.read_entry_bytes(entry)?;
        let decoded = decode_word_block(&bytes)?;
        validate_directory_header(decoded.header, entry)?;
        let decoded = Arc::new(decoded);
        cache_block(self.shared.store_id, Arc::clone(&decoded));
        Ok(decoded)
    }

    fn read_entry_bytes(&self, entry: BlockDirectoryEntry) -> StoreResult<Vec<u8>> {
        let mut bytes = vec![0u8; entry.block_len as usize];
        self.shared
            .read_backend
            .read()
            .unwrap()
            .read_exact_at(&mut bytes, entry.data_offset)?;
        Ok(bytes)
    }

    fn query_entry_words(
        &self,
        entry: BlockDirectoryEntry,
        start_ns: u64,
        end_ns: u64,
        max_context_words: usize,
    ) -> StoreResult<QueryBlockWords> {
        if let Some(block) = cached_block(self.shared.store_id, entry.sequence) {
            return Ok(QueryBlockWords::Cached(block));
        }
        if start_ns <= entry.first_timestamp_ns && end_ns >= entry.last_timestamp_ns {
            return self.read_cached_entry(entry).map(QueryBlockWords::Cached);
        }

        let bytes = self.read_entry_bytes(entry)?;
        let range = decode_word_block_range(&bytes, start_ns, end_ns, max_context_words)?;
        validate_directory_header(range.header, entry)?;
        Ok(QueryBlockWords::Partial {
            words: range.words,
            complete: range.complete,
        })
    }

    fn exact_context(
        &self,
        start_ns: u64,
        end_ns: u64,
    ) -> (u64, u64, Vec<BlockDirectoryEntry>, Arc<[Word]>) {
        let state = self.shared.state.read().unwrap();
        let first_intersecting = state
            .directory
            .partition_point(|entry| entry.last_timestamp_ns < start_ns);
        let first = first_intersecting.saturating_sub(1);
        let after_end = state
            .directory
            .partition_point(|entry| entry.first_timestamp_ns <= end_ns);
        let end = after_end.saturating_add(1).min(state.directory.len());
        let entries = if first < end {
            state.directory[first..end].to_vec()
        } else {
            Vec::new()
        };
        (
            state.generation,
            state.committed_word_count + state.hot_tail.len() as u64,
            entries,
            Arc::clone(&state.hot_tail),
        )
    }

    fn boundary_context(
        &self,
        timestamp_ns: u64,
        max_distance_ns: u64,
    ) -> (Vec<BlockDirectoryEntry>, Arc<[Word]>) {
        let lower = timestamp_ns.saturating_sub(max_distance_ns);
        let upper = timestamp_ns.saturating_add(max_distance_ns);
        let state = self.shared.state.read().unwrap();
        let first_intersecting = state
            .directory
            .partition_point(|entry| entry.last_timestamp_ns < lower);
        let first = first_intersecting.saturating_sub(1);
        let after_upper = state
            .directory
            .partition_point(|entry| entry.first_timestamp_ns <= upper);
        let end = after_upper.saturating_add(1).min(state.directory.len());
        let entries = if first < end {
            state.directory[first..end].to_vec()
        } else {
            Vec::new()
        };
        (entries, Arc::clone(&state.hot_tail))
    }
}

impl AnnotationQuery for IndexedAnnotationStore {
    fn metadata(&self) -> AnnotationStoreMetadata {
        let snapshot = self.snapshot();
        AnnotationStoreMetadata {
            generation: snapshot.metadata.generation,
            total_word_count: snapshot.metadata.committed_word_count
                + snapshot.metadata.hot_tail_word_count as u64,
            first_timestamp_ns: snapshot.metadata.first_timestamp_ns,
            last_timestamp_ns: snapshot.metadata.last_timestamp_ns,
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
        let mut buckets = {
            let state = self.shared.state.read().unwrap();
            let mut buckets = state
                .presence
                .presence_window_all(start_ns, end_ns, target_buckets);
            merge_hot_tail_presence(&mut buckets, &state.hot_tail);
            buckets
        };
        buckets.retain(|bucket| bucket.word_count > 0);
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
        let (generation, _total_words, entries, hot_tail) = self.exact_context(start_ns, end_ns);
        let mut context = Vec::with_capacity(max_words.saturating_add(2));
        let context_limit = max_words.saturating_add(2);
        let mut context_truncated = false;
        let mut found_successor = false;

        for entry in entries {
            let remaining = context_limit.saturating_sub(context.len()).max(1);
            let block = self
                .query_entry_words(entry, start_ns, end_ns, remaining)
                .map_err(query_store_error)?;
            append_query_context(
                block.words(),
                start_ns,
                end_ns,
                context_limit,
                &mut context,
                &mut context_truncated,
                &mut found_successor,
            );
            if !block.complete() {
                context_truncated = true;
            }
            if context_truncated || found_successor {
                break;
            }
        }
        if !context_truncated && !found_successor {
            append_query_context(
                &hot_tail,
                start_ns,
                end_ns,
                context_limit,
                &mut context,
                &mut context_truncated,
                &mut found_successor,
            );
        }

        let mut annotations = Vec::with_capacity(context.len().min(max_words));
        for (index, word) in context.iter().enumerate() {
            let end = if word.duration_ns != 0 {
                word.timestamp_ns.saturating_add(word.duration_ns)
            } else {
                context
                    .get(index + 1)
                    .map_or(word.timestamp_ns, |next| next.timestamp_ns)
            };
            if word.timestamp_ns <= end_ns && end >= start_ns {
                annotations.push(Annotation {
                    start_ns: word.timestamp_ns,
                    end_ns: end,
                    value: word.value,
                });
                if annotations.len() > max_words {
                    annotations.truncate(max_words);
                    context_truncated = true;
                    break;
                }
            }
        }

        Ok(ExactAnnotationWindow {
            annotations,
            complete: !context_truncated,
            generation,
        })
    }

    fn nearest_boundary(
        &self,
        timestamp_ns: u64,
        max_distance_ns: u64,
    ) -> AnnotationQueryResult<Option<u64>> {
        let (entries, hot_tail) = self.boundary_context(timestamp_ns, max_distance_ns);
        let lower = timestamp_ns.saturating_sub(max_distance_ns);
        let upper = timestamp_ns.saturating_add(max_distance_ns);
        let mut nearest = None;

        for entry in entries {
            let block = self
                .query_entry_words(entry, lower, upper, entry.word_count as usize + 2)
                .map_err(query_store_error)?;
            consider_word_boundaries(
                block.words(),
                lower,
                upper,
                timestamp_ns,
                max_distance_ns,
                &mut nearest,
            );
        }
        consider_word_boundaries(
            &hot_tail,
            lower,
            upper,
            timestamp_ns,
            max_distance_ns,
            &mut nearest,
        );
        Ok(nearest.map(|(boundary, _)| boundary))
    }
}

enum QueryBlockWords {
    Cached(Arc<DecodedWordBlock>),
    Partial { words: Vec<Word>, complete: bool },
}

fn merge_hot_tail_presence(buckets: &mut [WordPresenceBucket], words: &[Word]) {
    for word in words {
        let word_end = word.timestamp_ns.saturating_add(word.duration_ns);
        let first = buckets.partition_point(|bucket| bucket.end_ns < word.timestamp_ns);
        let end = buckets.partition_point(|bucket| bucket.start_ns <= word_end);
        for bucket in &mut buckets[first.min(end)..end] {
            bucket.word_count = bucket.word_count.saturating_add(1);
        }
    }
}

impl QueryBlockWords {
    fn words(&self) -> &[Word] {
        match self {
            Self::Cached(block) => &block.words,
            Self::Partial { words, .. } => words,
        }
    }

    fn complete(&self) -> bool {
        match self {
            Self::Cached(_) => true,
            Self::Partial { complete, .. } => *complete,
        }
    }
}

fn validate_directory_header(
    header: super::WordBlockHeader,
    entry: BlockDirectoryEntry,
) -> StoreResult<()> {
    if header.sequence != entry.sequence
        || header.first_timestamp_ns != entry.first_timestamp_ns
        || header.last_timestamp_ns != entry.last_timestamp_ns
        || header.block_len != entry.block_len
        || header.word_count != entry.word_count
        || header.value_bytes != entry.value_bytes
        || header.flags as u8 != entry.flags
    {
        return Err(StoreError::DirectoryMismatch(entry.sequence));
    }
    Ok(())
}

fn append_query_context(
    words: &[Word],
    start_ns: u64,
    end_ns: u64,
    context_limit: usize,
    context: &mut Vec<Word>,
    truncated: &mut bool,
    found_successor: &mut bool,
) {
    if words.is_empty() || *truncated || *found_successor {
        return;
    }
    let first_not_before = words.partition_point(|word| word.timestamp_ns < start_ns);
    let start = if context.is_empty() {
        first_not_before.saturating_sub(1)
    } else {
        0
    };
    for &word in &words[start..] {
        if context.len() >= context_limit {
            *truncated = true;
            return;
        }
        context.push(word);
        if word.timestamp_ns > end_ns {
            *found_successor = true;
            return;
        }
    }
}

fn consider_word_boundaries(
    words: &[Word],
    lower: u64,
    upper: u64,
    target: u64,
    max_distance: u64,
    nearest: &mut Option<(u64, u64)>,
) {
    if words.is_empty() {
        return;
    }
    let first = words
        .partition_point(|word| word.timestamp_ns < lower)
        .saturating_sub(1);
    let end = words
        .partition_point(|word| word.timestamp_ns <= upper)
        .saturating_add(1)
        .min(words.len());
    for word in &words[first..end] {
        consider_boundary(word.timestamp_ns, target, max_distance, nearest);
        if word.duration_ns != 0 {
            consider_boundary(
                word.timestamp_ns.saturating_add(word.duration_ns),
                target,
                max_distance,
                nearest,
            );
        }
    }
}

fn consider_boundary(
    boundary: u64,
    target: u64,
    max_distance: u64,
    nearest: &mut Option<(u64, u64)>,
) {
    let distance = boundary.abs_diff(target);
    if distance > max_distance {
        return;
    }
    if nearest.is_none_or(|(best_boundary, best_distance)| {
        distance < best_distance || (distance == best_distance && boundary < best_boundary)
    }) {
        *nearest = Some((boundary, distance));
    }
}

fn query_store_error(error: StoreError) -> AnnotationQueryError {
    AnnotationQueryError::Store(error.to_string())
}

/// Single-threaded append side of a live indexed annotation store.
pub struct IndexedAnnotationWriter {
    file: File,
    shared: Arc<StoreShared>,
    builder: WordBlockBuilder,
    encoded_block: Vec<u8>,
    next_sequence: u64,
    next_data_offset: u64,
    last_timestamp_ns: Option<u64>,
    words_since_tail_publish: usize,
    last_tail_publish: Instant,
    hot_tail_publish_words: usize,
    hot_tail_publish_interval: Duration,
    terminal: bool,
}

impl IndexedAnnotationWriter {
    pub fn create(config: LiveStoreConfig) -> StoreResult<(Self, IndexedAnnotationStore)> {
        if config.hot_tail_publish_words == 0 {
            return Err(StoreError::Codec(CodecError::InvalidConfiguration(
                "hot_tail_publish_words must be greater than zero",
            )));
        }
        std::fs::create_dir_all(&config.directory)?;
        let temporary = tempfile::Builder::new()
            .prefix("dsl-derived-")
            .suffix(".dwd.tmp")
            .tempfile_in(&config.directory)?;
        let (mut file, temp_path) = temporary.into_parts();
        let created_unix_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        file.write_all(
            &DataFileHeader {
                cache_key_prefix: config.cache_key_prefix,
                created_unix_ns,
                flags: 0,
            }
            .to_bytes(),
        )?;
        let reader = file.try_clone()?;
        let builder = WordBlockBuilder::new(config.block)?;
        let now = Instant::now();
        let last_tail_publish = now
            .checked_sub(config.hot_tail_publish_interval)
            .unwrap_or(now);
        static NEXT_STORE_ID: AtomicU64 = AtomicU64::new(1);
        let shared = Arc::new(StoreShared {
            state: RwLock::new(LiveState {
                directory: Vec::new(),
                presence: WordPresenceIndex::new(),
                generation: 0,
                committed_word_count: 0,
                committed_data_len: DATA_HEADER_SIZE as u64,
                committed_first_timestamp_ns: None,
                committed_last_timestamp_ns: None,
                hot_tail: Arc::from([]),
                status: StoreStatus::Live,
            }),
            read_backend: RwLock::new(ReadBackend::File(reader)),
            store_id: NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed),
            temp_path,
        });
        let store = IndexedAnnotationStore {
            shared: Arc::clone(&shared),
        };
        Ok((
            Self {
                file,
                shared,
                builder,
                encoded_block: Vec::new(),
                next_sequence: 0,
                next_data_offset: DATA_HEADER_SIZE as u64,
                last_timestamp_ns: None,
                words_since_tail_publish: 0,
                last_tail_publish,
                hot_tail_publish_words: config.hot_tail_publish_words,
                hot_tail_publish_interval: config.hot_tail_publish_interval,
                terminal: false,
            },
            store,
        ))
    }

    pub fn store(&self) -> IndexedAnnotationStore {
        IndexedAnnotationStore {
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn append(&mut self, word: Word) -> StoreResult<()> {
        self.append_batch(std::slice::from_ref(&word))
    }

    pub fn append_batch(&mut self, words: &[Word]) -> StoreResult<()> {
        self.ensure_live()?;
        let result = self.append_batch_inner(words);
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    pub fn publish_hot_tail(&mut self) -> StoreResult<()> {
        self.ensure_live()?;
        self.publish_hot_tail_inner();
        Ok(())
    }

    pub fn finish(&mut self) -> StoreResult<()> {
        self.ensure_live()?;
        let result = self.finish_inner();
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    /// Cancels without flushing or syncing. The temporary file is removed
    /// when the final writer/query handle is dropped.
    pub fn cancel(&mut self) -> StoreResult<()> {
        self.ensure_live()?;
        self.builder.clear();
        self.words_since_tail_publish = 0;
        let mut state = self.shared.state.write().unwrap();
        state.hot_tail = Arc::from([]);
        state.status = StoreStatus::Cancelled;
        state.generation += 1;
        self.terminal = true;
        Ok(())
    }

    fn append_batch_inner(&mut self, words: &[Word]) -> StoreResult<()> {
        for &word in words {
            if let Some(previous_timestamp_ns) = self.last_timestamp_ns
                && word.timestamp_ns < previous_timestamp_ns
            {
                return Err(StoreError::Codec(CodecError::OutOfOrder {
                    index: self.builder.len(),
                    previous_timestamp_ns,
                    timestamp_ns: word.timestamp_ns,
                }));
            }

            if self.builder.push(word)? == PushResult::BlockFull {
                self.commit_current_block()?;
                debug_assert_eq!(self.builder.push(word)?, PushResult::Appended);
            }
            self.last_timestamp_ns = Some(word.timestamp_ns);
            self.words_since_tail_publish += 1;
        }

        if !self.builder.is_empty()
            && (self.words_since_tail_publish >= self.hot_tail_publish_words
                || self.last_tail_publish.elapsed() >= self.hot_tail_publish_interval)
        {
            self.publish_hot_tail_inner();
        }
        Ok(())
    }

    fn finish_inner(&mut self) -> StoreResult<()> {
        self.commit_current_block()?;
        self.file.sync_data()?;
        // SAFETY: all appends are complete, sync_data returned successfully,
        // and `terminal` prevents any later mutation through this writer.
        let mmap = unsafe { MmapOptions::new().map(&self.file)? };
        *self.shared.read_backend.write().unwrap() = ReadBackend::Mmap(mmap);
        let mut state = self.shared.state.write().unwrap();
        state.status = StoreStatus::Finished;
        state.generation += 1;
        self.terminal = true;
        Ok(())
    }

    fn commit_current_block(&mut self) -> StoreResult<()> {
        if self.builder.is_empty() {
            return Ok(());
        }
        let metadata = self
            .builder
            .encode(self.next_sequence, &mut self.encoded_block)?;
        let header = metadata.header;
        let entry = BlockDirectoryEntry {
            sequence: header.sequence,
            first_timestamp_ns: header.first_timestamp_ns,
            last_timestamp_ns: header.last_timestamp_ns,
            data_offset: self.next_data_offset,
            block_len: header.block_len,
            word_count: header.word_count,
            value_bytes: header.value_bytes,
            flags: header.flags as u8,
        };
        let summary = WordSummaryRecord {
            start_ns: entry.first_timestamp_ns,
            end_ns: self
                .builder
                .words()
                .iter()
                .map(|word| word.timestamp_ns.saturating_add(word.duration_ns))
                .max()
                .unwrap_or(entry.last_timestamp_ns),
            word_count: u64::from(entry.word_count),
            first_block: entry.sequence,
            block_count: 1,
        };

        // File is unbuffered: once write_all returns, offset reads can see the
        // complete bytes. Publish the directory entry only after that point.
        self.file.write_all(&self.encoded_block)?;
        {
            let mut state = self.shared.state.write().unwrap();
            state.directory.push(entry);
            state.presence.push(summary);
            state.committed_word_count += u64::from(entry.word_count);
            state.committed_data_len = entry.data_offset + u64::from(entry.block_len);
            state
                .committed_first_timestamp_ns
                .get_or_insert(entry.first_timestamp_ns);
            state.committed_last_timestamp_ns = Some(entry.last_timestamp_ns);
            state.hot_tail = Arc::from([]);
            state.generation += 1;
        }

        self.next_sequence += 1;
        self.next_data_offset += u64::from(entry.block_len);
        self.builder.clear();
        self.words_since_tail_publish = 0;
        self.last_tail_publish = Instant::now();
        Ok(())
    }

    fn publish_hot_tail_inner(&mut self) {
        let snapshot: Arc<[Word]> = Arc::from(self.builder.words().to_vec());
        let mut state = self.shared.state.write().unwrap();
        state.hot_tail = snapshot;
        state.generation += 1;
        self.words_since_tail_publish = 0;
        self.last_tail_publish = Instant::now();
    }

    fn ensure_live(&self) -> StoreResult<()> {
        let status = self.shared.state.read().unwrap().status.clone();
        if status == StoreStatus::Live {
            Ok(())
        } else {
            Err(StoreError::NotLive(status))
        }
    }

    fn fail(&mut self, error: &StoreError) {
        self.shared.mark_failed(error.to_string());
        self.terminal = true;
    }
}

impl Drop for IndexedAnnotationWriter {
    fn drop(&mut self) {
        if self.terminal {
            return;
        }
        let mut state = self.shared.state.write().unwrap();
        if state.status == StoreStatus::Live {
            state.hot_tail = Arc::from([]);
            state.status = StoreStatus::Cancelled;
            state.generation += 1;
        }
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buffer, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buffer.is_empty() {
        let read = file.seek_read(buffer, offset)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "failed to fill derived-word block buffer",
            ));
        }
        offset += read as u64;
        buffer = &mut buffer[read..];
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn read_exact_at(_file: &File, _buffer: &mut [u8], _offset: u64) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "positional reads are unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::derived_word_store::cache::cache_contains;
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    fn test_config(directory: &Path) -> LiveStoreConfig {
        LiveStoreConfig {
            directory: directory.to_path_buf(),
            block: BlockCodecConfig {
                max_words: 16,
                ..BlockCodecConfig::default()
            },
            hot_tail_publish_words: 4,
            hot_tail_publish_interval: Duration::from_millis(10),
            ..LiveStoreConfig::default()
        }
    }

    #[test]
    fn finish_commits_partial_block_and_reads_it_by_directory_offset() {
        let directory = tempfile::tempdir().unwrap();
        let (mut writer, store) =
            IndexedAnnotationWriter::create(test_config(directory.path())).unwrap();
        let words: Vec<_> = (0..7)
            .map(|index| Word::spanning(index, index * 10, index % 3))
            .collect();

        writer.append_batch(&words).unwrap();
        assert_eq!(store.snapshot().hot_tail.as_ref(), words.as_slice());
        writer.finish().unwrap();

        let snapshot = store.snapshot();
        assert_eq!(snapshot.metadata.status, StoreStatus::Finished);
        assert_eq!(snapshot.metadata.committed_block_count, 1);
        assert_eq!(snapshot.metadata.committed_word_count, words.len() as u64);
        assert!(snapshot.metadata.mmap_backed);
        assert!(snapshot.hot_tail.is_empty());
        assert_eq!(store.read_committed_block(0).unwrap().words, words);
    }

    #[test]
    fn configured_boundaries_create_multiple_ordered_blocks() {
        let directory = tempfile::tempdir().unwrap();
        let (mut writer, store) =
            IndexedAnnotationWriter::create(test_config(directory.path())).unwrap();
        let words: Vec<_> = (0..41).map(|index| Word::new(index, index * 80)).collect();
        writer.append_batch(&words).unwrap();
        writer.finish().unwrap();

        let entries = store.directory();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].word_count, 16);
        assert_eq!(entries[1].word_count, 16);
        assert_eq!(entries[2].word_count, 9);
        let decoded: Vec<_> = (0..entries.len())
            .flat_map(|index| store.read_committed_block(index).unwrap().words)
            .collect();
        assert_eq!(decoded, words);
    }

    #[test]
    fn concurrent_readers_never_observe_partial_commits() {
        let directory = tempfile::tempdir().unwrap();
        let (mut writer, store) =
            IndexedAnnotationWriter::create(test_config(directory.path())).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let store = store.clone();
                let done = Arc::clone(&done);
                thread::spawn(move || {
                    let mut next_block = 0;
                    let mut words = Vec::new();
                    while !done.load(Ordering::Acquire)
                        || next_block < store.snapshot().metadata.committed_block_count
                    {
                        let block_count = store.snapshot().metadata.committed_block_count;
                        while next_block < block_count {
                            words.extend(store.read_committed_block(next_block).unwrap().words);
                            next_block += 1;
                        }
                        thread::yield_now();
                    }
                    words
                })
            })
            .collect();

        let expected: Vec<_> = (0..1_000)
            .map(|index| Word::new(index & 0xff, index * 80))
            .collect();
        for chunk in expected.chunks(37) {
            writer.append_batch(chunk).unwrap();
        }
        writer.finish().unwrap();
        done.store(true, Ordering::Release);

        for reader in readers {
            assert_eq!(reader.join().unwrap(), expected);
        }
    }

    #[test]
    fn out_of_order_input_fails_only_the_store() {
        let directory = tempfile::tempdir().unwrap();
        let (mut writer, store) =
            IndexedAnnotationWriter::create(test_config(directory.path())).unwrap();
        writer.append(Word::new(1, 10)).unwrap();
        assert!(matches!(
            writer.append(Word::new(2, 9)),
            Err(StoreError::Codec(CodecError::OutOfOrder { .. }))
        ));
        assert!(matches!(
            store.snapshot().metadata.status,
            StoreStatus::Failed(_)
        ));
        assert!(matches!(
            writer.append(Word::new(3, 11)),
            Err(StoreError::NotLive(StoreStatus::Failed(_)))
        ));
    }

    #[test]
    fn committed_read_error_is_reported_through_store_status() {
        let directory = tempfile::tempdir().unwrap();
        let (mut writer, store) =
            IndexedAnnotationWriter::create(test_config(directory.path())).unwrap();
        writer.append(Word::new(1, 0)).unwrap();
        writer.finish().unwrap();

        let entry = store.directory()[0];
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.temp_path())
            .unwrap();
        let payload_offset = entry.data_offset + super::super::BLOCK_HEADER_SIZE as u64;
        file.seek(SeekFrom::Start(payload_offset)).unwrap();
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 0x80;
        file.seek(SeekFrom::Start(payload_offset)).unwrap();
        file.write_all(&byte).unwrap();

        assert!(matches!(
            store.read_committed_block(0),
            Err(StoreError::Codec(CodecError::ChecksumMismatch { .. }))
        ));
        assert!(matches!(
            store.snapshot().metadata.status,
            StoreStatus::Failed(_)
        ));
    }

    #[test]
    fn cancellation_is_prompt_and_temp_file_lives_until_last_handle_drops() {
        let directory = tempfile::tempdir().unwrap();
        let (mut writer, store) =
            IndexedAnnotationWriter::create(test_config(directory.path())).unwrap();
        let path = store.temp_path().to_path_buf();
        writer.append(Word::new(1, 0)).unwrap();

        let start = Instant::now();
        writer.cancel().unwrap();
        drop(writer);
        assert!(start.elapsed() < Duration::from_millis(100));
        assert_eq!(store.snapshot().metadata.status, StoreStatus::Cancelled);
        assert!(path.exists());

        drop(store);
        assert!(!path.exists());
    }

    #[test]
    fn exact_query_combines_committed_blocks_and_the_live_hot_tail() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = test_config(directory.path());
        config.block.max_words = 4;
        config.hot_tail_publish_words = 1;
        let (mut writer, store) = IndexedAnnotationWriter::create(config).unwrap();
        let words: Vec<_> = (0..10)
            .map(|index| {
                if index == 6 {
                    Word::spanning(index, index * 10, 7)
                } else {
                    Word::new(index, index * 10)
                }
            })
            .collect();
        writer.append_batch(&words).unwrap();

        let result = store.exact_window(15, 75, 100).unwrap();
        assert!(result.complete);
        assert_eq!(result.annotations, direct_annotations(&words, 15, 75));
        assert_eq!(store.snapshot().metadata.committed_block_count, 2);
        assert_eq!(store.snapshot().metadata.hot_tail_word_count, 2);
    }

    #[test]
    fn exact_query_reports_an_incomplete_limited_window() {
        let directory = tempfile::tempdir().unwrap();
        let (mut writer, store) =
            IndexedAnnotationWriter::create(test_config(directory.path())).unwrap();
        let words: Vec<_> = (0..100).map(|index| Word::new(index, index * 10)).collect();
        writer.append_batch(&words).unwrap();
        writer.finish().unwrap();

        let result = store.exact_window(0, 1_000, 7).unwrap();
        assert!(!result.complete);
        assert_eq!(result.annotations.len(), 7);
    }

    #[test]
    fn nearest_boundary_checks_starts_explicit_ends_and_ties() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = test_config(directory.path());
        config.block.max_words = 2;
        config.hot_tail_publish_words = 1;
        let (mut writer, store) = IndexedAnnotationWriter::create(config).unwrap();
        writer
            .append_batch(&[Word::new(1, 10), Word::spanning(2, 30, 5), Word::new(3, 50)])
            .unwrap();

        assert_eq!(store.nearest_boundary(33, 10).unwrap(), Some(35));
        assert_eq!(store.nearest_boundary(20, 10).unwrap(), Some(10));
        assert_eq!(store.nearest_boundary(100, 20).unwrap(), None);
    }

    #[test]
    fn exact_and_boundary_queries_match_randomized_reference() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = test_config(directory.path());
        config.block.max_words = 31;
        let (mut writer, store) = IndexedAnnotationWriter::create(config).unwrap();
        let mut random = 0x243f_6a88_85a3_08d3u64;
        let mut timestamp = 0u64;
        let mut words = Vec::new();
        for index in 0..2_000 {
            timestamp += next_random(&mut random) % 20;
            let duration = if index % 23 == 0 {
                next_random(&mut random) % 10 + 1
            } else {
                0
            };
            words.push(Word::spanning(
                next_random(&mut random),
                timestamp,
                duration,
            ));
        }
        writer.append_batch(&words).unwrap();
        writer.finish().unwrap();

        for _ in 0..250 {
            let start = next_random(&mut random) % (timestamp + 100);
            let end = start + next_random(&mut random) % 500;
            let expected = direct_annotations(&words, start, end);
            let actual = store.exact_window(start, end, 10_000).unwrap();
            assert!(actual.complete);
            assert_eq!(actual.annotations, expected, "window {start}..={end}");

            let target = next_random(&mut random) % (timestamp + 100);
            let max_distance = next_random(&mut random) % 100;
            assert_eq!(
                store.nearest_boundary(target, max_distance).unwrap(),
                direct_nearest_boundary(&words, target, max_distance),
                "target={target}, max_distance={max_distance}"
            );
        }
    }

    #[test]
    fn exact_query_populates_the_process_decoded_block_cache() {
        let directory = tempfile::tempdir().unwrap();
        let (mut writer, store) =
            IndexedAnnotationWriter::create(test_config(directory.path())).unwrap();
        writer
            .append_batch(&[Word::new(1, 10), Word::new(2, 20)])
            .unwrap();
        writer.finish().unwrap();
        assert!(!cache_contains(store.shared.store_id, 0));

        store.exact_window(0, 30, 10).unwrap();
        assert!(cache_contains(store.shared.store_id, 0));
    }

    #[test]
    fn presence_query_uses_summaries_and_hot_tail_without_decoding_blocks() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = test_config(directory.path());
        config.block.max_words = 4;
        config.hot_tail_publish_words = 1;
        let (mut writer, store) = IndexedAnnotationWriter::create(config).unwrap();
        let words: Vec<_> = (0..10).map(|index| Word::new(index, index * 100)).collect();
        writer.append_batch(&words).unwrap();
        assert_eq!(store.snapshot().metadata.committed_block_count, 2);
        assert_eq!(store.snapshot().metadata.hot_tail_word_count, 2);
        assert!(!cache_contains(store.shared.store_id, 0));
        assert!(!cache_contains(store.shared.store_id, 1));

        let buckets = store.presence_window(0, 999, 10).unwrap();
        assert!(buckets.len() <= 10);
        assert!(buckets.iter().any(|bucket| bucket.end_ns >= 900));
        assert!(!cache_contains(store.shared.store_id, 0));
        assert!(!cache_contains(store.shared.store_id, 1));
    }

    #[test]
    fn presence_query_does_not_fill_a_large_inter_block_gap() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = test_config(directory.path());
        config.block.max_words = 64;
        config.block.max_inter_word_gap_ns = 100;
        config.hot_tail_publish_words = 1;
        let (mut writer, store) = IndexedAnnotationWriter::create(config).unwrap();
        writer
            .append_batch(&[Word::new(1, 0), Word::new(2, 10), Word::new(3, 10_000)])
            .unwrap();

        let buckets = store.presence_window(0, 10_009, 10).unwrap();
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].start_ns, 0);
        assert_eq!(buckets[1].end_ns, 10_009);
    }

    fn direct_annotations(words: &[Word], start_ns: u64, end_ns: u64) -> Vec<Annotation> {
        words
            .iter()
            .enumerate()
            .filter_map(|(index, word)| {
                let annotation_end = if word.duration_ns != 0 {
                    word.timestamp_ns.saturating_add(word.duration_ns)
                } else {
                    words
                        .get(index + 1)
                        .map_or(word.timestamp_ns, |next| next.timestamp_ns)
                };
                (word.timestamp_ns <= end_ns && annotation_end >= start_ns).then_some(Annotation {
                    start_ns: word.timestamp_ns,
                    end_ns: annotation_end,
                    value: word.value,
                })
            })
            .collect()
    }

    fn direct_nearest_boundary(words: &[Word], target: u64, max_distance: u64) -> Option<u64> {
        words
            .iter()
            .flat_map(|word| {
                [
                    Some(word.timestamp_ns),
                    (word.duration_ns != 0)
                        .then_some(word.timestamp_ns.saturating_add(word.duration_ns)),
                ]
            })
            .flatten()
            .filter_map(|boundary| {
                let distance = boundary.abs_diff(target);
                (distance <= max_distance).then_some((boundary, distance))
            })
            .min_by_key(|&(boundary, distance)| (distance, boundary))
            .map(|(boundary, _)| boundary)
    }

    fn next_random(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }
}
