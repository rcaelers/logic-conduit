//! Native sequential file-backed live-capture store.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::{
    CaptureChunk, CaptureChunkPayload, CaptureChunkWriter, CaptureSessionId, CaptureWriteError,
};

use super::{
    CaptureCursorItem, CaptureStoreCursor, CaptureStoreDescriptor, CaptureStoreError,
    CaptureStoreManifest, CaptureStoreResult, CaptureStoreSnapshot,
};

const DATA_FILE_NAME: &str = "capture.data";
const COMMIT_FILE_NAME: &str = "capture.commits";
const MANIFEST_FILE_NAME: &str = "capture.manifest";
const MANIFEST_TEMP_FILE_NAME: &str = "capture.manifest.tmp";
const COMMIT_MAGIC: &[u8; 8] = b"DSLCMT01";
const MANIFEST_MAGIC: &[u8; 8] = b"DSLSES01";
const STORE_FORMAT_VERSION: u16 = 1;
const COMMIT_HEADER_SIZE: u16 = 32;
const COMMIT_RECORD_SIZE: u16 = 48;
const PACKED_LSB_FIRST_ENCODING: u8 = 1;
const DEFAULT_COMMIT_BATCH_CHUNKS: usize = 16;

#[derive(Clone, Debug)]
pub struct NativeCaptureStoreConfig {
    directory: PathBuf,
    descriptor: CaptureStoreDescriptor,
    commit_batch_chunks: usize,
}

impl NativeCaptureStoreConfig {
    pub fn new(directory: impl Into<PathBuf>, descriptor: CaptureStoreDescriptor) -> Self {
        Self {
            directory: directory.into(),
            descriptor,
            commit_batch_chunks: DEFAULT_COMMIT_BATCH_CHUNKS,
        }
    }

    pub fn with_commit_batch_chunks(mut self, chunks: usize) -> CaptureStoreResult<Self> {
        if chunks == 0 {
            return Err(CaptureStoreError::InvalidConfig(
                "commit batch size must be non-zero".into(),
            ));
        }
        self.commit_batch_chunks = chunks;
        Ok(self)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn descriptor(&self) -> &CaptureStoreDescriptor {
        &self.descriptor
    }
}

#[derive(Debug)]
struct StoreState {
    committed_chunks: u64,
    committed_samples: u64,
    committed_data_bytes: u64,
    writer_open: bool,
    finalized: bool,
    writer_failure: Option<String>,
}

#[derive(Debug)]
struct SharedStore {
    directory: PathBuf,
    descriptor: CaptureStoreDescriptor,
    state: Mutex<StoreState>,
    changed: Condvar,
}

impl SharedStore {
    fn snapshot(&self) -> CaptureStoreSnapshot {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        CaptureStoreSnapshot {
            committed_chunks: state.committed_chunks,
            committed_samples: state.committed_samples,
            committed_data_bytes: state.committed_data_bytes,
            writer_open: state.writer_open,
            writer_failed: state.writer_failure.is_some(),
            finalized: state.finalized,
            resident_commit_records: 0,
        }
    }

    fn manifest(&self) -> CaptureStoreManifest {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        CaptureStoreManifest {
            descriptor: self.descriptor.clone(),
            committed_chunks: state.committed_chunks,
            committed_samples: state.committed_samples,
            committed_data_bytes: state.committed_data_bytes,
        }
    }

    fn record_writer_failure(&self, error: &CaptureStoreError) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.writer_failure = Some(error.to_string());
        self.changed.notify_all();
    }

    fn close_writer(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.writer_open = false;
        self.changed.notify_all();
    }
}

#[derive(Clone)]
pub struct NativeCaptureStore {
    shared: Arc<SharedStore>,
}

impl NativeCaptureStore {
    pub fn create(
        config: NativeCaptureStoreConfig,
    ) -> CaptureStoreResult<(Self, NativeCaptureStoreWriter)> {
        fs::create_dir_all(&config.directory)?;
        let data_path = config.directory.join(DATA_FILE_NAME);
        let commit_path = config.directory.join(COMMIT_FILE_NAME);
        let manifest_path = config.directory.join(MANIFEST_FILE_NAME);
        if data_path.exists() || commit_path.exists() || manifest_path.exists() {
            return Err(CaptureStoreError::InvalidConfig(format!(
                "capture-store directory is not empty: {}",
                config.directory.display()
            )));
        }

        let data_file = create_new_file(&data_path)?;
        let mut commit_file = create_new_file(&commit_path)?;
        write_commit_header(&mut commit_file, config.descriptor.session_id())?;
        commit_file.sync_data()?;

        let shared = Arc::new(SharedStore {
            directory: config.directory,
            descriptor: config.descriptor,
            state: Mutex::new(StoreState {
                committed_chunks: 0,
                committed_samples: 0,
                committed_data_bytes: 0,
                writer_open: true,
                finalized: false,
                writer_failure: None,
            }),
            changed: Condvar::new(),
        });
        let store = Self {
            shared: Arc::clone(&shared),
        };
        let writer = NativeCaptureStoreWriter {
            shared,
            data_file,
            commit_file,
            pending: Vec::with_capacity(config.commit_batch_chunks),
            commit_batch_chunks: config.commit_batch_chunks,
            next_sequence: 0,
            next_sample: 0,
            next_data_offset: 0,
            terminal: false,
        };
        Ok((store, writer))
    }

    pub fn descriptor(&self) -> &CaptureStoreDescriptor {
        &self.shared.descriptor
    }

    pub fn directory(&self) -> &Path {
        &self.shared.directory
    }

    pub fn snapshot(&self) -> CaptureStoreSnapshot {
        self.shared.snapshot()
    }

    pub fn open_cursor(&self) -> CaptureStoreResult<NativeCaptureCursor> {
        NativeCaptureCursor::open(Arc::clone(&self.shared))
    }

    pub fn finalize(&self) -> CaptureStoreResult<NativeFinalizedCapture> {
        let manifest = {
            let state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.writer_open {
                return Err(CaptureStoreError::WriterStillOpen);
            }
            if state.finalized {
                return Err(CaptureStoreError::AlreadyFinalized);
            }
            if let Some(error) = &state.writer_failure {
                return Err(CaptureStoreError::WriterFailed(error.clone()));
            }
            CaptureStoreManifest {
                descriptor: self.shared.descriptor.clone(),
                committed_chunks: state.committed_chunks,
                committed_samples: state.committed_samples,
                committed_data_bytes: state.committed_data_bytes,
            }
        };
        write_manifest(&self.shared.directory, &manifest)?;
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.finalized = true;
        self.shared.changed.notify_all();
        drop(state);
        Ok(NativeFinalizedCapture {
            shared: Arc::clone(&self.shared),
        })
    }
}

#[derive(Clone)]
pub struct NativeFinalizedCapture {
    shared: Arc<SharedStore>,
}

impl NativeFinalizedCapture {
    pub fn open(directory: impl Into<PathBuf>) -> CaptureStoreResult<Self> {
        let directory = directory.into();
        let manifest = read_manifest(&directory.join(MANIFEST_FILE_NAME))?;
        validate_finalized_files(&directory, &manifest)?;
        Ok(Self {
            shared: Arc::new(SharedStore {
                directory,
                descriptor: manifest.descriptor,
                state: Mutex::new(StoreState {
                    committed_chunks: manifest.committed_chunks,
                    committed_samples: manifest.committed_samples,
                    committed_data_bytes: manifest.committed_data_bytes,
                    writer_open: false,
                    finalized: true,
                    writer_failure: None,
                }),
                changed: Condvar::new(),
            }),
        })
    }

    pub fn manifest(&self) -> CaptureStoreManifest {
        self.shared.manifest()
    }

    pub fn directory(&self) -> &Path {
        &self.shared.directory
    }

    pub fn open_cursor(&self) -> CaptureStoreResult<NativeCaptureCursor> {
        NativeCaptureCursor::open(Arc::clone(&self.shared))
    }
}

#[derive(Clone, Copy, Debug)]
struct CommitRecord {
    sequence: u64,
    start_sample: u64,
    sample_count: u64,
    data_offset: u64,
    data_len: u64,
    bit_offset: u8,
    encoding: u8,
}

pub struct NativeCaptureStoreWriter {
    shared: Arc<SharedStore>,
    data_file: File,
    commit_file: File,
    pending: Vec<CommitRecord>,
    commit_batch_chunks: usize,
    next_sequence: u64,
    next_sample: u64,
    next_data_offset: u64,
    terminal: bool,
}

impl NativeCaptureStoreWriter {
    fn append_inner(&mut self, chunk: CaptureChunk) -> CaptureStoreResult<()> {
        if self.terminal {
            return Err(CaptureStoreError::WriterFailed(
                "capture-store writer is already finished".into(),
            ));
        }
        if chunk.session_id() != self.shared.descriptor.session_id() {
            return Err(CaptureStoreError::InvalidChunk(format!(
                "session {} does not match {}",
                chunk.session_id(),
                self.shared.descriptor.session_id()
            )));
        }
        if chunk.channels() != self.shared.descriptor.channels() {
            return Err(CaptureStoreError::InvalidChunk(
                "chunk channel table differs from the session descriptor".into(),
            ));
        }
        if chunk.sequence() != self.next_sequence {
            return Err(CaptureStoreError::InvalidChunk(format!(
                "chunk sequence {} follows {}",
                chunk.sequence(),
                self.next_sequence
            )));
        }
        if chunk.start_sample() != self.next_sample {
            return Err(CaptureStoreError::InvalidChunk(format!(
                "chunk starts at sample {}, expected {}",
                chunk.start_sample(),
                self.next_sample
            )));
        }
        let (bytes, bit_offset, encoding) = match chunk.payload() {
            CaptureChunkPayload::PackedLsbFirst { bytes, bit_offset } => {
                (bytes.as_ref(), *bit_offset, PACKED_LSB_FIRST_ENCODING)
            }
        };
        let data_len = bytes.len() as u64;
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| CaptureStoreError::InvalidChunk("chunk sequence overflow".into()))?;
        let next_data_offset = self
            .next_data_offset
            .checked_add(data_len)
            .ok_or_else(|| CaptureStoreError::InvalidChunk("data offset overflow".into()))?;
        self.data_file.write_all(bytes)?;
        self.pending.push(CommitRecord {
            sequence: chunk.sequence(),
            start_sample: chunk.start_sample(),
            sample_count: chunk.sample_count(),
            data_offset: self.next_data_offset,
            data_len,
            bit_offset,
            encoding,
        });
        self.next_sequence = next_sequence;
        self.next_sample = chunk.end_sample();
        self.next_data_offset = next_data_offset;
        if self.pending.len() >= self.commit_batch_chunks {
            self.flush_pending()?;
        }
        Ok(())
    }

    fn flush_pending(&mut self) -> CaptureStoreResult<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.data_file.sync_data()?;
        let mut encoded = Vec::with_capacity(self.pending.len() * usize::from(COMMIT_RECORD_SIZE));
        for record in &self.pending {
            encode_commit_record(*record, &mut encoded);
        }
        self.commit_file.write_all(&encoded)?;
        self.commit_file.sync_data()?;
        let last = *self.pending.last().expect("pending is non-empty");
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.committed_chunks = last
            .sequence
            .checked_add(1)
            .ok_or_else(|| CaptureStoreError::Corrupt("committed chunk count overflow".into()))?;
        state.committed_samples = last
            .start_sample
            .checked_add(last.sample_count)
            .ok_or_else(|| CaptureStoreError::Corrupt("committed sample count overflow".into()))?;
        state.committed_data_bytes = last
            .data_offset
            .checked_add(last.data_len)
            .ok_or_else(|| CaptureStoreError::Corrupt("committed data length overflow".into()))?;
        self.pending.clear();
        self.shared.changed.notify_all();
        Ok(())
    }

    fn finish_inner(&mut self) -> CaptureStoreResult<()> {
        if self.terminal {
            return Ok(());
        }
        let result = self.flush_pending();
        self.terminal = true;
        result
    }

    fn map_write_result(&self, result: CaptureStoreResult<()>) -> Result<(), CaptureWriteError> {
        result.map_err(|error| CaptureWriteError::Rejected(error.to_string()))
    }
}

impl CaptureChunkWriter for NativeCaptureStoreWriter {
    fn append(&mut self, chunk: CaptureChunk) -> Result<(), CaptureWriteError> {
        let result = self.append_inner(chunk);
        if let Err(error) = &result {
            self.shared.record_writer_failure(error);
        }
        self.map_write_result(result)
    }

    fn finish(&mut self) -> Result<(), CaptureWriteError> {
        let result = self.finish_inner();
        if let Err(error) = &result {
            self.shared.record_writer_failure(error);
        }
        self.map_write_result(result)
    }
}

impl Drop for NativeCaptureStoreWriter {
    fn drop(&mut self) {
        if !self.terminal
            && let Err(error) = self.finish_inner()
        {
            self.shared.record_writer_failure(&error);
        }
        self.shared.close_writer();
    }
}

pub struct NativeCaptureCursor {
    shared: Arc<SharedStore>,
    data_file: File,
    commit_file: File,
    next_sequence: u64,
    next_sample: u64,
}

impl NativeCaptureCursor {
    fn open(shared: Arc<SharedStore>) -> CaptureStoreResult<Self> {
        let data_file = File::open(shared.directory.join(DATA_FILE_NAME))?;
        let mut commit_file = File::open(shared.directory.join(COMMIT_FILE_NAME))?;
        validate_commit_header(&mut commit_file, shared.descriptor.session_id())?;
        Ok(Self {
            shared,
            data_file,
            commit_file,
            next_sequence: 0,
            next_sample: 0,
        })
    }

    fn next_available(&mut self, wait: Option<Duration>) -> CaptureStoreResult<CaptureCursorItem> {
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if self.next_sequence >= state.committed_chunks {
                if let Some(error) = &state.writer_failure {
                    return Err(CaptureStoreError::WriterFailed(error.clone()));
                }
                if !state.writer_open {
                    return Ok(CaptureCursorItem::End);
                }
                let Some(timeout) = wait else {
                    return Ok(CaptureCursorItem::Pending);
                };
                let (new_state, _) = self
                    .shared
                    .changed
                    .wait_timeout_while(state, timeout, |current| {
                        self.next_sequence >= current.committed_chunks && current.writer_open
                    })
                    .unwrap_or_else(|error| error.into_inner());
                state = new_state;
                if self.next_sequence >= state.committed_chunks {
                    if let Some(error) = &state.writer_failure {
                        return Err(CaptureStoreError::WriterFailed(error.clone()));
                    }
                    return Ok(if state.writer_open {
                        CaptureCursorItem::Pending
                    } else {
                        CaptureCursorItem::End
                    });
                }
            }
        }
        self.read_next_chunk().map(CaptureCursorItem::Chunk)
    }

    fn read_next_chunk(&mut self) -> CaptureStoreResult<CaptureChunk> {
        let offset = u64::from(COMMIT_HEADER_SIZE)
            .checked_add(
                self.next_sequence
                    .checked_mul(u64::from(COMMIT_RECORD_SIZE))
                    .ok_or_else(|| CaptureStoreError::Corrupt("commit offset overflow".into()))?,
            )
            .ok_or_else(|| CaptureStoreError::Corrupt("commit offset overflow".into()))?;
        self.commit_file.seek(SeekFrom::Start(offset))?;
        let mut bytes = [0_u8; COMMIT_RECORD_SIZE as usize];
        self.commit_file.read_exact(&mut bytes)?;
        let record = decode_commit_record(&bytes)?;
        if record.sequence != self.next_sequence || record.start_sample != self.next_sample {
            return Err(CaptureStoreError::Corrupt(format!(
                "commit {} has sequence {} and start {}, expected start {}",
                self.next_sequence, record.sequence, record.start_sample, self.next_sample
            )));
        }
        if record.encoding != PACKED_LSB_FIRST_ENCODING {
            return Err(CaptureStoreError::Corrupt(format!(
                "unsupported chunk encoding {}",
                record.encoding
            )));
        }
        let data_len = usize::try_from(record.data_len)
            .map_err(|_| CaptureStoreError::Corrupt("chunk data length is too large".into()))?;
        let mut data = vec![0_u8; data_len];
        self.data_file.seek(SeekFrom::Start(record.data_offset))?;
        self.data_file.read_exact(&mut data)?;
        let chunk = CaptureChunk::packed_lsb_first(
            self.shared.descriptor.session_id(),
            record.sequence,
            record.start_sample,
            record.sample_count,
            self.shared.descriptor.channel_table(),
            data,
            record.bit_offset,
        )
        .map_err(|error| CaptureStoreError::Corrupt(error.to_string()))?;
        self.next_sequence += 1;
        self.next_sample = chunk.end_sample();
        Ok(chunk)
    }
}

impl CaptureStoreCursor for NativeCaptureCursor {
    fn next(&mut self) -> CaptureStoreResult<CaptureCursorItem> {
        self.next_available(None)
    }

    fn wait_next(&mut self, timeout: Duration) -> CaptureStoreResult<CaptureCursorItem> {
        self.next_available(Some(timeout))
    }

    fn next_sequence(&self) -> u64 {
        self.next_sequence
    }
}

fn create_new_file(path: &Path) -> CaptureStoreResult<File> {
    Ok(OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)?)
}

fn write_commit_header(file: &mut File, session_id: CaptureSessionId) -> CaptureStoreResult<()> {
    let mut bytes = Vec::with_capacity(usize::from(COMMIT_HEADER_SIZE));
    bytes.extend_from_slice(COMMIT_MAGIC);
    bytes.extend_from_slice(&STORE_FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&COMMIT_HEADER_SIZE.to_le_bytes());
    bytes.extend_from_slice(&COMMIT_RECORD_SIZE.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&session_id.get().to_le_bytes());
    debug_assert_eq!(bytes.len(), usize::from(COMMIT_HEADER_SIZE));
    file.write_all(&bytes)?;
    Ok(())
}

fn validate_commit_header(
    file: &mut File,
    expected_session: CaptureSessionId,
) -> CaptureStoreResult<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = [0_u8; COMMIT_HEADER_SIZE as usize];
    file.read_exact(&mut bytes)?;
    if &bytes[0..8] != COMMIT_MAGIC
        || get_u16(&bytes, 8)? != STORE_FORMAT_VERSION
        || get_u16(&bytes, 10)? != COMMIT_HEADER_SIZE
        || get_u16(&bytes, 12)? != COMMIT_RECORD_SIZE
        || get_u128(&bytes, 16)? != expected_session.get()
    {
        return Err(CaptureStoreError::Corrupt(
            "commit header is incompatible with the session".into(),
        ));
    }
    Ok(())
}

fn encode_commit_record(record: CommitRecord, output: &mut Vec<u8>) {
    output.extend_from_slice(&record.sequence.to_le_bytes());
    output.extend_from_slice(&record.start_sample.to_le_bytes());
    output.extend_from_slice(&record.sample_count.to_le_bytes());
    output.extend_from_slice(&record.data_offset.to_le_bytes());
    output.extend_from_slice(&record.data_len.to_le_bytes());
    output.push(record.bit_offset);
    output.push(record.encoding);
    output.extend_from_slice(&[0_u8; 6]);
}

fn decode_commit_record(bytes: &[u8]) -> CaptureStoreResult<CommitRecord> {
    if bytes.len() != usize::from(COMMIT_RECORD_SIZE) {
        return Err(CaptureStoreError::Corrupt(
            "commit record has an invalid length".into(),
        ));
    }
    Ok(CommitRecord {
        sequence: get_u64(bytes, 0)?,
        start_sample: get_u64(bytes, 8)?,
        sample_count: get_u64(bytes, 16)?,
        data_offset: get_u64(bytes, 24)?,
        data_len: get_u64(bytes, 32)?,
        bit_offset: bytes[40],
        encoding: bytes[41],
    })
}

fn write_manifest(directory: &Path, manifest: &CaptureStoreManifest) -> CaptureStoreResult<()> {
    let bytes = encode_manifest(manifest)?;
    let temp_path = directory.join(MANIFEST_TEMP_FILE_NAME);
    let final_path = directory.join(MANIFEST_FILE_NAME);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp_path)?;
    file.write_all(&bytes)?;
    file.sync_data()?;
    drop(file);
    fs::rename(temp_path, final_path)?;
    Ok(())
}

fn encode_manifest(manifest: &CaptureStoreManifest) -> CaptureStoreResult<Vec<u8>> {
    let channel_count = u32::try_from(manifest.descriptor.channels().len())
        .map_err(|_| CaptureStoreError::InvalidConfig("too many capture channels".into()))?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MANIFEST_MAGIC);
    bytes.extend_from_slice(&STORE_FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&manifest.descriptor.session_id().get().to_le_bytes());
    bytes.extend_from_slice(&manifest.committed_chunks.to_le_bytes());
    bytes.extend_from_slice(&manifest.committed_samples.to_le_bytes());
    bytes.extend_from_slice(&manifest.committed_data_bytes.to_le_bytes());
    bytes.extend_from_slice(&channel_count.to_le_bytes());
    for channel in manifest.descriptor.channels() {
        let value = channel.as_str().as_bytes();
        let len = u32::try_from(value.len())
            .map_err(|_| CaptureStoreError::InvalidConfig("channel ID is too long".into()))?;
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(value);
    }
    Ok(bytes)
}

fn read_manifest(path: &Path) -> CaptureStoreResult<CaptureStoreManifest> {
    let bytes = fs::read(path)?;
    if bytes.len() < 56
        || &bytes[0..8] != MANIFEST_MAGIC
        || get_u16(&bytes, 8)? != STORE_FORMAT_VERSION
    {
        return Err(CaptureStoreError::Corrupt(
            "manifest header is invalid".into(),
        ));
    }
    let session_id = CaptureSessionId::new(get_u128(&bytes, 12)?);
    let committed_chunks = get_u64(&bytes, 28)?;
    let committed_samples = get_u64(&bytes, 36)?;
    let committed_data_bytes = get_u64(&bytes, 44)?;
    let channel_count = get_u32(&bytes, 52)? as usize;
    let mut offset = 56;
    let mut channels = Vec::with_capacity(channel_count);
    for _ in 0..channel_count {
        let len = get_u32(&bytes, offset)? as usize;
        offset = offset
            .checked_add(4)
            .ok_or_else(|| CaptureStoreError::Corrupt("manifest offset overflow".into()))?;
        let end = offset
            .checked_add(len)
            .ok_or_else(|| CaptureStoreError::Corrupt("manifest offset overflow".into()))?;
        let value = bytes
            .get(offset..end)
            .ok_or_else(|| CaptureStoreError::Corrupt("truncated channel ID".into()))?;
        let value = std::str::from_utf8(value)
            .map_err(|_| CaptureStoreError::Corrupt("channel ID is not UTF-8".into()))?;
        channels.push(crate::CaptureChannelId::new(value));
        offset = end;
    }
    if offset != bytes.len() {
        return Err(CaptureStoreError::Corrupt(
            "manifest contains trailing bytes".into(),
        ));
    }
    Ok(CaptureStoreManifest {
        descriptor: CaptureStoreDescriptor::new(session_id, channels)?,
        committed_chunks,
        committed_samples,
        committed_data_bytes,
    })
}

fn validate_finalized_files(
    directory: &Path,
    manifest: &CaptureStoreManifest,
) -> CaptureStoreResult<()> {
    let mut commit_file = File::open(directory.join(COMMIT_FILE_NAME))?;
    validate_commit_header(&mut commit_file, manifest.descriptor.session_id())?;
    let expected_commit_len = u64::from(COMMIT_HEADER_SIZE)
        .checked_add(
            manifest
                .committed_chunks
                .checked_mul(u64::from(COMMIT_RECORD_SIZE))
                .ok_or_else(|| CaptureStoreError::Corrupt("commit length overflow".into()))?,
        )
        .ok_or_else(|| CaptureStoreError::Corrupt("commit length overflow".into()))?;
    if commit_file.metadata()?.len() != expected_commit_len {
        return Err(CaptureStoreError::Corrupt(
            "commit file length differs from the manifest".into(),
        ));
    }
    let data_len = File::open(directory.join(DATA_FILE_NAME))?.metadata()?.len();
    if data_len != manifest.committed_data_bytes {
        return Err(CaptureStoreError::Corrupt(
            "data file length differs from the manifest".into(),
        ));
    }
    Ok(())
}

fn get_u16(bytes: &[u8], offset: usize) -> CaptureStoreResult<u16> {
    let value = get_bytes(bytes, offset, 2, "u16")?;
    Ok(u16::from_le_bytes(value.try_into().expect("slice is two bytes")))
}

fn get_u32(bytes: &[u8], offset: usize) -> CaptureStoreResult<u32> {
    let value = get_bytes(bytes, offset, 4, "u32")?;
    Ok(u32::from_le_bytes(
        value.try_into().expect("slice is four bytes"),
    ))
}

fn get_u64(bytes: &[u8], offset: usize) -> CaptureStoreResult<u64> {
    let value = get_bytes(bytes, offset, 8, "u64")?;
    Ok(u64::from_le_bytes(
        value.try_into().expect("slice is eight bytes"),
    ))
}

fn get_u128(bytes: &[u8], offset: usize) -> CaptureStoreResult<u128> {
    let value = get_bytes(bytes, offset, 16, "u128")?;
    Ok(u128::from_le_bytes(
        value.try_into().expect("slice is sixteen bytes"),
    ))
}

fn get_bytes<'a>(
    bytes: &'a [u8],
    offset: usize,
    len: usize,
    name: &str,
) -> CaptureStoreResult<&'a [u8]> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| CaptureStoreError::Corrupt(format!("{name} offset overflow")))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| CaptureStoreError::Corrupt(format!("truncated {name}")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;

    use crate::{
        CaptureChannelId, CaptureChunk, CaptureChunkWriter, CaptureSessionId,
        CaptureStoreCursor,
    };

    use super::{
        CaptureCursorItem, CaptureStoreDescriptor, CaptureStoreError, NativeCaptureStore,
        NativeCaptureStoreConfig, NativeFinalizedCapture,
    };

    fn descriptor() -> CaptureStoreDescriptor {
        CaptureStoreDescriptor::new(
            CaptureSessionId::new(0xabc),
            vec![
                CaptureChannelId::new("bank-a:7"),
                CaptureChannelId::new("bank-c:2"),
                CaptureChannelId::new("aux:19"),
            ],
        )
        .unwrap()
    }

    fn chunk(
        descriptor: &CaptureStoreDescriptor,
        sequence: u64,
        start: u64,
        samples: u64,
    ) -> CaptureChunk {
        let bit_offset = ((sequence * 3 + 1) % 8) as u8;
        let bit_count = samples as usize * descriptor.channels().len();
        let mut bytes = vec![0_u8; (usize::from(bit_offset) + bit_count).div_ceil(8)];
        for bit in 0..bit_count {
            if (bit + start as usize + sequence as usize).is_multiple_of(3) {
                let absolute = usize::from(bit_offset) + bit;
                bytes[absolute / 8] |= 1 << (absolute % 8);
            }
        }
        CaptureChunk::packed_lsb_first(
            descriptor.session_id(),
            sequence,
            start,
            samples,
            Arc::<[CaptureChannelId]>::from(descriptor.channels().to_vec()),
            bytes,
            bit_offset,
        )
        .unwrap()
    }

    #[test]
    fn finalized_store_reopens_every_unaligned_chunk() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor.clone())
            .with_commit_batch_chunks(2)
            .unwrap();
        let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
        let expected = [3_u64, 5, 2, 7, 4];
        let mut start = 0;
        for (sequence, samples) in expected.into_iter().enumerate() {
            writer
                .append(chunk(&descriptor, sequence as u64, start, samples))
                .unwrap();
            start += samples;
        }
        writer.finish().unwrap();
        drop(writer);
        let finalized = store.finalize().unwrap();
        let reopened = NativeFinalizedCapture::open(finalized.directory()).unwrap();
        assert_eq!(reopened.manifest().committed_chunks, expected.len() as u64);
        assert_eq!(reopened.manifest().committed_samples, start);

        let mut cursor = reopened.open_cursor().unwrap();
        for (sequence, samples) in expected.into_iter().enumerate() {
            let CaptureCursorItem::Chunk(actual) = cursor.next().unwrap() else {
                panic!("missing chunk {sequence}");
            };
            let expected = chunk(
                &descriptor,
                sequence as u64,
                actual.start_sample(),
                samples,
            );
            assert_eq!(actual, expected);
        }
        assert_eq!(cursor.next().unwrap(), CaptureCursorItem::End);
    }

    #[test]
    fn paused_cursor_does_not_retain_commit_records_or_block_writer() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor.clone())
            .with_commit_batch_chunks(32)
            .unwrap();
        let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
        let mut cursor = store.open_cursor().unwrap();
        for sequence in 0..2_048_u64 {
            writer
                .append(chunk(&descriptor, sequence, sequence, 1))
                .unwrap();
        }
        writer.finish().unwrap();
        drop(writer);

        let snapshot = store.snapshot();
        assert_eq!(snapshot.committed_chunks, 2_048);
        assert_eq!(snapshot.committed_samples, 2_048);
        assert_eq!(snapshot.resident_commit_records, 0);
        assert_eq!(cursor.next_sequence(), 0);
        assert!(matches!(cursor.next().unwrap(), CaptureCursorItem::Chunk(_)));
        assert_eq!(cursor.next_sequence(), 1);
    }

    #[test]
    fn partial_batch_is_not_visible_until_writer_finishes_it() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor.clone())
            .with_commit_batch_chunks(4)
            .unwrap();
        let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
        let mut cursor = store.open_cursor().unwrap();
        writer.append(chunk(&descriptor, 0, 0, 3)).unwrap();

        assert_eq!(store.snapshot().committed_chunks, 0);
        assert_eq!(cursor.next().unwrap(), CaptureCursorItem::Pending);
        writer.finish().unwrap();
        assert_eq!(store.snapshot().committed_chunks, 1);
        assert!(matches!(cursor.next().unwrap(), CaptureCursorItem::Chunk(_)));
        drop(writer);
        assert_eq!(cursor.next().unwrap(), CaptureCursorItem::End);
    }

    #[test]
    fn invalid_append_marks_the_session_failed_for_cursor_and_finalize() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor.clone());
        let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
        let mut cursor = store.open_cursor().unwrap();

        assert!(writer.append(chunk(&descriptor, 1, 0, 3)).is_err());
        drop(writer);
        assert!(store.snapshot().writer_failed);
        assert!(matches!(
            cursor.next(),
            Err(CaptureStoreError::WriterFailed(_))
        ));
        assert!(matches!(
            store.finalize(),
            Err(CaptureStoreError::WriterFailed(_))
        ));
    }
}
