use super::{
    BlockCodecConfig, BlockDirectoryEntry, CodecError, DATA_HEADER_SIZE, DataFileHeader,
    DecodedWordBlock, PushResult, WordBlockBuilder, decode_word_block,
};
use crate::runtime::Word;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
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
    pub status: StoreStatus,
}

#[derive(Debug, Clone)]
pub struct LiveStoreSnapshot {
    pub metadata: LiveStoreMetadata,
    pub hot_tail: Arc<[Word]>,
}

struct LiveState {
    directory: Vec<BlockDirectoryEntry>,
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
    reader: File,
    // Kept after the file handles so they close before TempPath removes the file.
    temp_path: tempfile::TempPath,
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

        let result = self.read_entry(entry);
        if let Err(error) = &result {
            self.shared.mark_failed(error.to_string());
        }
        result
    }

    fn read_entry(&self, entry: BlockDirectoryEntry) -> StoreResult<DecodedWordBlock> {
        let mut bytes = vec![0u8; entry.block_len as usize];
        read_exact_at(&self.shared.reader, &mut bytes, entry.data_offset)?;
        let decoded = decode_word_block(&bytes)?;
        let header = decoded.header;
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
        Ok(decoded)
    }
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
        let shared = Arc::new(StoreShared {
            state: RwLock::new(LiveState {
                directory: Vec::new(),
                generation: 0,
                committed_word_count: 0,
                committed_data_len: DATA_HEADER_SIZE as u64,
                committed_first_timestamp_ns: None,
                committed_last_timestamp_ns: None,
                hot_tail: Arc::from([]),
                status: StoreStatus::Live,
            }),
            reader,
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

        // File is unbuffered: once write_all returns, offset reads can see the
        // complete bytes. Publish the directory entry only after that point.
        self.file.write_all(&self.encoded_block)?;
        {
            let mut state = self.shared.state.write().unwrap();
            state.directory.push(entry);
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
}
