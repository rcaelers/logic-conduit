//! Native sequential file-backed live-capture store.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{
    CaptureChunk, CaptureChunkPayload, CaptureChunkWriter, CaptureSampledChannel,
    CaptureSampledWindow, CaptureSessionId, CaptureSessionPlan, CaptureTransition,
    CaptureWriteError, Error,
};

use super::{
    CaptureCursorItem, CaptureReclamationReport, CaptureRecoveryReport, CaptureSessionMetadata,
    CaptureSessionOutcome, CaptureStoreCursor, CaptureStoreDescriptor, CaptureStoreError,
    CaptureStoreManifest, CaptureStoreResult, CaptureStoreSnapshot, CaptureTimelineMetadata,
};

const DATA_FILE_NAME: &str = "capture.data";
const COMMIT_FILE_NAME: &str = "capture.commits";
const LIVE_COMMIT_FILE_NAME: &str = "capture.live.commits";
const MANIFEST_FILE_NAME: &str = "capture.manifest";
const MANIFEST_TEMP_FILE_NAME: &str = "capture.manifest.tmp";
const PLAN_FILE_NAME: &str = "capture.plan.json";
const PLAN_TEMP_FILE_NAME: &str = "capture.plan.json.tmp";
const SESSION_FILE_NAME: &str = "capture.session.json";
const SESSION_TEMP_FILE_NAME: &str = "capture.session.json.tmp";
const RECLAIM_FILE_NAME: &str = "capture.reclaim.json";
const RECLAIM_TEMP_FILE_NAME: &str = "capture.reclaim.json.tmp";
const RECLAIM_DATA_FILE_NAME: &str = "capture.data.reclaim";
const RECLAIM_COMMIT_FILE_NAME: &str = "capture.commits.reclaim";
const RECLAIM_OLD_DATA_FILE_NAME: &str = "capture.data.old";
const RECLAIM_OLD_COMMIT_FILE_NAME: &str = "capture.commits.old";
const COMMIT_MAGIC: &[u8; 8] = b"DSLCMT01";
const MANIFEST_MAGIC: &[u8; 8] = b"DSLSES01";
const LEGACY_COMMIT_FORMAT_VERSION: u16 = 1;
const COMMIT_FORMAT_VERSION: u16 = 2;
const MANIFEST_FORMAT_VERSION: u16 = 1;
const SESSION_FORMAT_VERSION: u16 = 2;
const RECLAIM_FORMAT_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReclamationDurableStep {
    TransactionWritten,
    DataBackedUp,
    DataInstalled,
    CommitsBackedUp,
    CommitsInstalled,
    PlanInstalled,
    MetadataInstalled,
    ManifestInstalled,
    BackupsRemoved,
    TransactionRemoved,
}
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
    visible_chunks: u64,
    visible_samples: u64,
    visible_data_bytes: u64,
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

    fn live_snapshot(&self) -> CaptureStoreSnapshot {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        CaptureStoreSnapshot {
            committed_chunks: state.visible_chunks,
            committed_samples: state.visible_samples,
            committed_data_bytes: state.visible_data_bytes,
            writer_open: state.writer_open,
            writer_failed: state.writer_failure.is_some(),
            finalized: state.finalized,
            resident_commit_records: 0,
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
        let live_commit_path = config.directory.join(LIVE_COMMIT_FILE_NAME);
        let manifest_path = config.directory.join(MANIFEST_FILE_NAME);
        let plan_path = config.directory.join(PLAN_FILE_NAME);
        let plan_temp_path = config.directory.join(PLAN_TEMP_FILE_NAME);
        let session_path = config.directory.join(SESSION_FILE_NAME);
        let session_temp_path = config.directory.join(SESSION_TEMP_FILE_NAME);
        if data_path.exists()
            || commit_path.exists()
            || live_commit_path.exists()
            || manifest_path.exists()
            || plan_path.exists()
            || plan_temp_path.exists()
            || session_path.exists()
            || session_temp_path.exists()
            || config.directory.join(RECLAIM_FILE_NAME).exists()
            || config.directory.join(RECLAIM_TEMP_FILE_NAME).exists()
            || config.directory.join(RECLAIM_DATA_FILE_NAME).exists()
            || config.directory.join(RECLAIM_COMMIT_FILE_NAME).exists()
            || config.directory.join(RECLAIM_OLD_DATA_FILE_NAME).exists()
            || config.directory.join(RECLAIM_OLD_COMMIT_FILE_NAME).exists()
        {
            return Err(CaptureStoreError::InvalidConfig(format!(
                "capture-store directory is not empty: {}",
                config.directory.display()
            )));
        }

        let data_file = create_new_file(&data_path)?;
        let mut commit_file = create_new_file(&commit_path)?;
        write_commit_header(&mut commit_file, config.descriptor.session_id())?;
        commit_file.sync_data()?;
        let mut live_commit_file = create_new_file(&live_commit_path)?;
        write_commit_header(&mut live_commit_file, config.descriptor.session_id())?;
        write_session_metadata(
            &config.directory,
            &CaptureSessionMetadata {
                descriptor: config.descriptor.clone(),
                timeline: None,
                outcome: CaptureSessionOutcome::InProgress,
                created_unix_ns: unix_ns(),
                accessed_unix_ns: unix_ns(),
                recording_origin: None,
                retained_start_sample: 0,
                kept: false,
            },
        )?;

        let shared = Arc::new(SharedStore {
            directory: config.directory,
            descriptor: config.descriptor,
            state: Mutex::new(StoreState {
                visible_chunks: 0,
                visible_samples: 0,
                visible_data_bytes: 0,
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
            live_commit_file,
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
        NativeCaptureCursor::open(Arc::clone(&self.shared), CursorVisibility::Durable)
    }

    pub(crate) fn open_live_cursor(&self) -> CaptureStoreResult<NativeCaptureCursor> {
        NativeCaptureCursor::open(Arc::clone(&self.shared), CursorVisibility::Written)
    }

    pub fn open_random_reader(&self) -> CaptureStoreResult<NativeCaptureRandomReader> {
        NativeCaptureRandomReader::open(Arc::clone(&self.shared), CursorVisibility::Durable)
    }

    pub(crate) fn open_live_random_reader(&self) -> CaptureStoreResult<NativeCaptureRandomReader> {
        NativeCaptureRandomReader::open(Arc::clone(&self.shared), CursorVisibility::Written)
    }

    pub fn write_session_plan(&self, plan: &CaptureSessionPlan) -> CaptureStoreResult<()> {
        {
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
        }
        write_session_plan(&self.shared.directory, plan)
    }

    pub fn write_timeline_metadata(
        &self,
        timeline: CaptureTimelineMetadata,
    ) -> CaptureStoreResult<()> {
        if timeline.channel_names().len() != self.shared.descriptor.channels().len() {
            return Err(CaptureStoreError::InvalidConfig(format!(
                "capture timeline has {} channel names for {} channels",
                timeline.channel_names().len(),
                self.shared.descriptor.channels().len()
            )));
        }
        {
            let state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.finalized {
                return Err(CaptureStoreError::AlreadyFinalized);
            }
            if let Some(error) = &state.writer_failure {
                return Err(CaptureStoreError::WriterFailed(error.clone()));
            }
        }
        let mut metadata = read_session_metadata(&self.shared.directory)?.ok_or_else(|| {
            CaptureStoreError::Corrupt("capture session metadata is missing".into())
        })?;
        metadata.timeline = Some(timeline);
        metadata.accessed_unix_ns = unix_ns();
        write_session_metadata(&self.shared.directory, &metadata)
    }

    pub fn finalize(&self) -> CaptureStoreResult<NativeFinalizedCapture> {
        self.finalize_with_outcome(CaptureSessionOutcome::Complete, None)
    }

    pub fn finalize_with_outcome(
        &self,
        outcome: CaptureSessionOutcome,
        recording_origin: Option<u64>,
    ) -> CaptureStoreResult<NativeFinalizedCapture> {
        self.finalize_with_details(outcome, recording_origin, None)
    }

    pub fn finalize_with_details(
        &self,
        outcome: CaptureSessionOutcome,
        recording_origin: Option<u64>,
        trigger_sample: Option<u64>,
    ) -> CaptureStoreResult<NativeFinalizedCapture> {
        if !outcome.is_terminal() || outcome == CaptureSessionOutcome::Corrupt {
            return Err(CaptureStoreError::InvalidConfig(
                "a finalized capture requires a non-corrupt terminal outcome".into(),
            ));
        }
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
        let mut metadata = read_session_metadata(&self.shared.directory)?.ok_or_else(|| {
            CaptureStoreError::Corrupt("capture session metadata is missing".into())
        })?;
        if let Some(trigger_sample) = trigger_sample {
            if trigger_sample >= manifest.committed_samples {
                return Err(CaptureStoreError::InvalidConfig(format!(
                    "trigger sample {trigger_sample} is outside the {}-sample capture",
                    manifest.committed_samples
                )));
            }
            let timeline = metadata.timeline.as_mut().ok_or_else(|| {
                CaptureStoreError::InvalidConfig(
                    "a trigger sample requires durable capture timeline metadata".into(),
                )
            })?;
            timeline.set_trigger_sample(Some(trigger_sample));
        }
        metadata.outcome = outcome;
        metadata.recording_origin = recording_origin;
        metadata.accessed_unix_ns = unix_ns();
        write_session_metadata(&self.shared.directory, &metadata)?;
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
        let _ = read_session_plan(&directory)?;
        if let Some(metadata) = read_session_metadata(&directory)?
            && metadata.descriptor != manifest.descriptor
        {
            return Err(CaptureStoreError::Corrupt(
                "capture session metadata differs from the finalized manifest".into(),
            ));
        }
        Ok(Self {
            shared: Arc::new(SharedStore {
                directory,
                descriptor: manifest.descriptor,
                state: Mutex::new(StoreState {
                    visible_chunks: manifest.committed_chunks,
                    visible_samples: manifest.committed_samples,
                    visible_data_bytes: manifest.committed_data_bytes,
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

    pub fn recover(
        directory: impl Into<PathBuf>,
    ) -> CaptureStoreResult<(Self, CaptureRecoveryReport)> {
        let directory = directory.into();
        finish_pending_reclamation(&directory)?;
        remove_if_exists(&directory.join(RECLAIM_TEMP_FILE_NAME))?;
        remove_if_exists(&directory.join(RECLAIM_DATA_FILE_NAME))?;
        remove_if_exists(&directory.join(RECLAIM_COMMIT_FILE_NAME))?;
        if directory.join(MANIFEST_FILE_NAME).is_file() {
            let capture = Self::open(&directory)?;
            let mut recovered = false;
            if let Some(mut metadata) = capture.session_metadata()?
                && metadata.outcome == CaptureSessionOutcome::InProgress
            {
                metadata.outcome = CaptureSessionOutcome::Incomplete;
                metadata.accessed_unix_ns = unix_ns();
                write_session_metadata(&directory, &metadata)?;
                recovered = true;
            }
            return Ok((
                capture,
                CaptureRecoveryReport {
                    recovered,
                    ..CaptureRecoveryReport::default()
                },
            ));
        }

        let mut metadata = read_session_metadata(&directory)?.ok_or_else(|| {
            CaptureStoreError::Corrupt(
                "interrupted capture has no durable session metadata".into(),
            )
        })?;
        let mut commit_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join(COMMIT_FILE_NAME))?;
        let commit_format_version =
            validate_commit_header(&mut commit_file, metadata.descriptor.session_id())?;
        let mut data_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join(DATA_FILE_NAME))?;
        let commit_len = commit_file.metadata()?.len();
        if commit_len < u64::from(COMMIT_HEADER_SIZE) {
            return Err(CaptureStoreError::Corrupt(
                "interrupted commit log is shorter than its header".into(),
            ));
        }
        let record_bytes = commit_len - u64::from(COMMIT_HEADER_SIZE);
        let complete_records = record_bytes / u64::from(COMMIT_RECORD_SIZE);
        let committed_log_len = u64::from(COMMIT_HEADER_SIZE)
            .checked_add(
                complete_records
                    .checked_mul(u64::from(COMMIT_RECORD_SIZE))
                    .ok_or_else(|| CaptureStoreError::Corrupt("commit length overflow".into()))?,
            )
            .ok_or_else(|| CaptureStoreError::Corrupt("commit length overflow".into()))?;
        let data_len = data_file.metadata()?.len();
        let validation_store = SharedStore {
            directory: directory.clone(),
            descriptor: metadata.descriptor.clone(),
            state: Mutex::new(StoreState {
                visible_chunks: complete_records,
                visible_samples: 0,
                visible_data_bytes: 0,
                committed_chunks: complete_records,
                committed_samples: 0,
                committed_data_bytes: 0,
                writer_open: false,
                finalized: false,
                writer_failure: None,
            }),
            changed: Condvar::new(),
        };
        let mut expected_sample = 0_u64;
        let mut expected_data_offset = 0_u64;
        for sequence in 0..complete_records {
            let record = read_commit_record_at(
                &mut commit_file,
                sequence,
                commit_format_version,
            )?;
            if record.start_sample != expected_sample || record.data_offset != expected_data_offset
            {
                return Err(CaptureStoreError::Corrupt(format!(
                    "interrupted commit {sequence} is not contiguous"
                )));
            }
            let record_end = record
                .data_offset
                .checked_add(record.data_len)
                .ok_or_else(|| CaptureStoreError::Corrupt("data extent overflow".into()))?;
            if record_end > data_len {
                return Err(CaptureStoreError::Corrupt(format!(
                    "committed chunk {sequence} extends beyond durable data"
                )));
            }
            let _ = read_record_chunk(
                &validation_store,
                &mut data_file,
                commit_format_version,
                record,
            )?;
            expected_sample = record
                .start_sample
                .checked_add(record.sample_count)
                .ok_or_else(|| CaptureStoreError::Corrupt("sample extent overflow".into()))?;
            expected_data_offset = record_end;
        }

        let truncated_commit_bytes = commit_len - committed_log_len;
        let truncated_data_bytes = data_len.saturating_sub(expected_data_offset);
        if truncated_commit_bytes != 0 {
            commit_file.set_len(committed_log_len)?;
            commit_file.sync_data()?;
        }
        if truncated_data_bytes != 0 {
            data_file.set_len(expected_data_offset)?;
            data_file.sync_data()?;
        }
        metadata.outcome = match metadata.outcome {
            CaptureSessionOutcome::Corrupt => {
                return Err(CaptureStoreError::Corrupt(
                    "capture session was previously marked corrupt".into(),
                ));
            }
            CaptureSessionOutcome::InProgress => CaptureSessionOutcome::Incomplete,
            terminal => terminal,
        };
        metadata.accessed_unix_ns = unix_ns();
        write_session_metadata(&directory, &metadata)?;
        let manifest = CaptureStoreManifest {
            descriptor: metadata.descriptor.clone(),
            committed_chunks: complete_records,
            committed_samples: expected_sample,
            committed_data_bytes: expected_data_offset,
        };
        write_manifest(&directory, &manifest)?;
        remove_if_exists(&directory.join(MANIFEST_TEMP_FILE_NAME))?;
        remove_if_exists(&directory.join(PLAN_TEMP_FILE_NAME))?;
        remove_if_exists(&directory.join(SESSION_TEMP_FILE_NAME))?;
        let _ = read_session_plan(&directory)?;
        let capture = Self {
            shared: Arc::new(SharedStore {
                directory,
                descriptor: manifest.descriptor,
                state: Mutex::new(StoreState {
                    visible_chunks: manifest.committed_chunks,
                    visible_samples: manifest.committed_samples,
                    visible_data_bytes: manifest.committed_data_bytes,
                    committed_chunks: manifest.committed_chunks,
                    committed_samples: manifest.committed_samples,
                    committed_data_bytes: manifest.committed_data_bytes,
                    writer_open: false,
                    finalized: true,
                    writer_failure: None,
                }),
                changed: Condvar::new(),
            }),
        };
        Ok((
            capture,
            CaptureRecoveryReport {
                recovered: true,
                truncated_data_bytes,
                truncated_commit_bytes,
            },
        ))
    }

    pub(super) fn reclaim_directory_before(
        directory: &Path,
        safe_sample: u64,
    ) -> CaptureStoreResult<(Self, CaptureReclamationReport)> {
        Self::reclaim_directory_before_with_hook(directory, safe_sample, |_| Ok(()))
    }

    fn reclaim_directory_before_with_hook(
        directory: &Path,
        safe_sample: u64,
        mut after_step: impl FnMut(ReclamationDurableStep) -> CaptureStoreResult<()>,
    ) -> CaptureStoreResult<(Self, CaptureReclamationReport)> {
        finish_pending_reclamation(directory)?;
        let capture = Self::open(directory)?;
        let manifest = capture.manifest();
        if safe_sample == 0 || manifest.committed_chunks == 0 {
            return Ok((capture, CaptureReclamationReport::default()));
        }
        let mut metadata = capture.session_metadata()?.ok_or_else(|| {
            CaptureStoreError::InvalidConfig(
                "legacy capture sessions cannot execute bounded reclamation".into(),
            )
        })?;
        let mut commit_file = File::open(directory.join(COMMIT_FILE_NAME))?;
        let commit_format_version =
            validate_commit_header(&mut commit_file, manifest.descriptor.session_id())?;
        let mut first_retained_sequence = 0_u64;
        let mut reclaimed_sample = 0_u64;
        let mut reclaimed_data_bytes = 0_u64;
        while first_retained_sequence < manifest.committed_chunks {
            let record = read_commit_record_at(
                &mut commit_file,
                first_retained_sequence,
                commit_format_version,
            )?;
            let end = record
                .start_sample
                .checked_add(record.sample_count)
                .ok_or_else(|| CaptureStoreError::Corrupt("sample extent overflow".into()))?;
            if end > safe_sample {
                break;
            }
            first_retained_sequence += 1;
            reclaimed_sample = end;
            reclaimed_data_bytes = record
                .data_offset
                .checked_add(record.data_len)
                .ok_or_else(|| CaptureStoreError::Corrupt("data extent overflow".into()))?;
        }
        if first_retained_sequence == 0 {
            return Ok((capture, CaptureReclamationReport::default()));
        }

        for name in [RECLAIM_DATA_FILE_NAME, RECLAIM_COMMIT_FILE_NAME] {
            remove_if_exists(&directory.join(name))?;
        }
        let mut source_data = File::open(directory.join(DATA_FILE_NAME))?;
        let mut reclaimed_data = create_new_file(&directory.join(RECLAIM_DATA_FILE_NAME))?;
        let mut reclaimed_commits = create_new_file(&directory.join(RECLAIM_COMMIT_FILE_NAME))?;
        write_commit_header(&mut reclaimed_commits, manifest.descriptor.session_id())?;
        let mut output_data_offset = 0_u64;
        for input_sequence in first_retained_sequence..manifest.committed_chunks {
            let output_sequence = input_sequence - first_retained_sequence;
            let record = read_commit_record_at(
                &mut commit_file,
                input_sequence,
                commit_format_version,
            )?;
            let data_len = usize::try_from(record.data_len)
                .map_err(|_| CaptureStoreError::Corrupt("chunk data is too large".into()))?;
            let mut data = vec![0_u8; data_len];
            source_data.seek(SeekFrom::Start(record.data_offset))?;
            source_data.read_exact(&mut data)?;
            validate_record_checksum(commit_format_version, record, &data)?;
            reclaimed_data.write_all(&data)?;
            let mut output = CommitRecord {
                sequence: output_sequence,
                start_sample: record.start_sample.saturating_sub(reclaimed_sample),
                sample_count: record.sample_count,
                data_offset: output_data_offset,
                data_len: record.data_len,
                bit_offset: record.bit_offset,
                encoding: record.encoding,
                checksum: 0,
            };
            output.checksum = commit_record_checksum(output, &data);
            let mut encoded = Vec::with_capacity(usize::from(COMMIT_RECORD_SIZE));
            encode_commit_record(output, &mut encoded);
            reclaimed_commits.write_all(&encoded)?;
            output_data_offset = output_data_offset
                .checked_add(record.data_len)
                .ok_or_else(|| CaptureStoreError::Corrupt("data extent overflow".into()))?;
        }
        reclaimed_data.sync_data()?;
        reclaimed_commits.sync_data()?;
        drop(reclaimed_data);
        drop(reclaimed_commits);

        metadata.retained_start_sample = metadata
            .retained_start_sample
            .saturating_add(reclaimed_sample);
        metadata.recording_origin = metadata
            .recording_origin
            .map(|origin| origin.saturating_sub(reclaimed_sample));
        if let Some(timeline) = &mut metadata.timeline {
            timeline.set_trigger_sample(
                timeline
                    .trigger_sample()
                    .map(|trigger| trigger.saturating_sub(reclaimed_sample)),
            );
        }
        metadata.accessed_unix_ns = unix_ns();
        let mut plan = capture.session_plan()?;
        if let Some(plan) = &mut plan
            && let Some(crate::TriggerPlacement::SamplesBefore(before)) =
                &mut plan.policy.effective.trigger_placement
        {
            *before = before.saturating_sub(reclaimed_sample);
        }
        let transaction = PersistedReclamation {
            format_version: RECLAIM_FORMAT_VERSION,
            committed_chunks: manifest
                .committed_chunks
                .saturating_sub(first_retained_sequence),
            committed_samples: manifest.committed_samples.saturating_sub(reclaimed_sample),
            committed_data_bytes: manifest
                .committed_data_bytes
                .saturating_sub(reclaimed_data_bytes),
            metadata: PersistedSessionMetadata::from_metadata(&metadata),
            plan,
        };
        write_reclamation(directory, &transaction)?;
        after_step(ReclamationDurableStep::TransactionWritten)?;
        finish_pending_reclamation_with_hook(directory, &mut after_step)?;
        remove_waveform_summaries(directory)?;
        let capture = Self::open(directory)?;
        Ok((
            capture,
            CaptureReclamationReport {
                reclaimed_chunks: first_retained_sequence,
                reclaimed_samples: reclaimed_sample,
                reclaimed_data_bytes,
            },
        ))
    }

    pub fn manifest(&self) -> CaptureStoreManifest {
        self.shared.manifest()
    }

    pub fn directory(&self) -> &Path {
        &self.shared.directory
    }

    pub fn open_cursor(&self) -> CaptureStoreResult<NativeCaptureCursor> {
        NativeCaptureCursor::open(Arc::clone(&self.shared), CursorVisibility::Durable)
    }

    pub fn open_random_reader(&self) -> CaptureStoreResult<NativeCaptureRandomReader> {
        NativeCaptureRandomReader::open(Arc::clone(&self.shared), CursorVisibility::Durable)
    }

    pub fn session_plan(&self) -> CaptureStoreResult<Option<CaptureSessionPlan>> {
        read_session_plan(&self.shared.directory)
    }

    pub fn session_metadata(&self) -> CaptureStoreResult<Option<CaptureSessionMetadata>> {
        read_session_metadata(&self.shared.directory)
    }

    pub fn store_handle(&self) -> NativeCaptureStore {
        NativeCaptureStore {
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn set_kept(&self, kept: bool) -> CaptureStoreResult<()> {
        let Some(mut metadata) = self.session_metadata()? else {
            return Err(CaptureStoreError::InvalidConfig(
                "legacy capture sessions cannot be marked as kept".into(),
            ));
        };
        metadata.kept = kept;
        metadata.accessed_unix_ns = unix_ns();
        write_session_metadata(&self.shared.directory, &metadata)
    }

    pub fn touch(&self) -> CaptureStoreResult<()> {
        let Some(mut metadata) = self.session_metadata()? else {
            return Ok(());
        };
        metadata.accessed_unix_ns = unix_ns();
        write_session_metadata(&self.shared.directory, &metadata)
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
    checksum: u32,
}

pub struct NativeCaptureStoreWriter {
    shared: Arc<SharedStore>,
    data_file: File,
    commit_file: File,
    live_commit_file: File,
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
        let mut record = CommitRecord {
            sequence: chunk.sequence(),
            start_sample: chunk.start_sample(),
            sample_count: chunk.sample_count(),
            data_offset: self.next_data_offset,
            data_len,
            bit_offset,
            encoding,
            checksum: 0,
        };
        record.checksum = commit_record_checksum(record, bytes);
        let mut encoded = Vec::with_capacity(usize::from(COMMIT_RECORD_SIZE));
        encode_commit_record(record, &mut encoded);
        self.live_commit_file.write_all(&encoded)?;
        self.pending.push(record);
        self.next_sequence = next_sequence;
        self.next_sample = chunk.end_sample();
        self.next_data_offset = next_data_offset;
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.visible_chunks = self.next_sequence;
            state.visible_samples = self.next_sample;
            state.visible_data_bytes = self.next_data_offset;
            self.shared.changed.notify_all();
        }
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

#[derive(Clone, Copy)]
enum CursorVisibility {
    Durable,
    Written,
}

pub struct NativeCaptureCursor {
    shared: Arc<SharedStore>,
    data_file: File,
    commit_file: File,
    next_sequence: u64,
    next_sample: u64,
    commit_format_version: u16,
    visibility: CursorVisibility,
}

impl NativeCaptureCursor {
    fn open(
        shared: Arc<SharedStore>,
        visibility: CursorVisibility,
    ) -> CaptureStoreResult<Self> {
        let data_file = File::open(shared.directory.join(DATA_FILE_NAME))?;
        let finalized = shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .finalized;
        let commit_name = match visibility {
            CursorVisibility::Written
                if !finalized && shared.directory.join(LIVE_COMMIT_FILE_NAME).is_file() =>
            {
                LIVE_COMMIT_FILE_NAME
            }
            CursorVisibility::Durable | CursorVisibility::Written => COMMIT_FILE_NAME,
        };
        let mut commit_file = File::open(shared.directory.join(commit_name))?;
        let commit_format_version =
            validate_commit_header(&mut commit_file, shared.descriptor.session_id())?;
        Ok(Self {
            shared,
            data_file,
            commit_file,
            next_sequence: 0,
            next_sample: 0,
            commit_format_version,
            visibility,
        })
    }

    fn available_chunks(&self, state: &StoreState) -> u64 {
        match self.visibility {
            CursorVisibility::Durable => state.committed_chunks,
            CursorVisibility::Written => state.visible_chunks,
        }
    }

    fn next_available(&mut self, wait: Option<Duration>) -> CaptureStoreResult<CaptureCursorItem> {
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if self.next_sequence >= self.available_chunks(&state) {
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
                        self.next_sequence >= self.available_chunks(current) && current.writer_open
                    })
                    .unwrap_or_else(|error| error.into_inner());
                state = new_state;
                if self.next_sequence >= self.available_chunks(&state) {
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
        validate_record_checksum(self.commit_format_version, record, &data)?;
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

/// Random-access exact reader over the committed prefix of a native live
/// store. Commit records are fixed-size and sample ordered, so locating the
/// first intersecting chunk is logarithmic and no acquisition-sized index is
/// retained in memory.
pub struct NativeCaptureRandomReader {
    shared: Arc<SharedStore>,
    data_file: File,
    commit_file: File,
    commit_format_version: u16,
    visibility: CursorVisibility,
}

impl NativeCaptureRandomReader {
    fn open(
        shared: Arc<SharedStore>,
        visibility: CursorVisibility,
    ) -> CaptureStoreResult<Self> {
        let data_file = File::open(shared.directory.join(DATA_FILE_NAME))?;
        let finalized = shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .finalized;
        let commit_name = match visibility {
            CursorVisibility::Written
                if !finalized && shared.directory.join(LIVE_COMMIT_FILE_NAME).is_file() =>
            {
                LIVE_COMMIT_FILE_NAME
            }
            CursorVisibility::Durable | CursorVisibility::Written => COMMIT_FILE_NAME,
        };
        let mut commit_file = File::open(shared.directory.join(commit_name))?;
        let commit_format_version =
            validate_commit_header(&mut commit_file, shared.descriptor.session_id())?;
        Ok(Self {
            shared,
            data_file,
            commit_file,
            commit_format_version,
            visibility,
        })
    }

    pub fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
    ) -> crate::Result<CaptureSampledWindow> {
        let snapshot = match self.visibility {
            CursorVisibility::Durable => self.shared.snapshot(),
            CursorVisibility::Written => self.shared.live_snapshot(),
        };
        if start_sample >= end_sample || end_sample > snapshot.committed_samples {
            return Err(Error::OutOfBounds(end_sample));
        }
        for &channel in channels {
            if channel >= self.shared.descriptor.channels().len() {
                return Err(Error::InvalidProbe(channel));
            }
        }

        let mut sampled = channels
            .iter()
            .map(|&channel| CaptureSampledChannel {
                channel,
                name: self.shared.descriptor.channels()[channel].to_string(),
                initial: false,
                transitions: Vec::new(),
                waveform: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut current = vec![None; channels.len()];
        let first_sequence = self
            .sequence_containing(start_sample, snapshot.committed_chunks)
            .map_err(store_as_capture_error)?;

        for sequence in first_sequence..snapshot.committed_chunks {
            let record = read_commit_record_at(
                &mut self.commit_file,
                sequence,
                self.commit_format_version,
            )
                .map_err(store_as_capture_error)?;
            if record.start_sample >= end_sample {
                break;
            }
            let chunk = read_record_chunk(
                &self.shared,
                &mut self.data_file,
                self.commit_format_version,
                record,
            )
                .map_err(store_as_capture_error)?;
            let chunk_start = start_sample.max(chunk.start_sample());
            let chunk_end = end_sample.min(chunk.end_sample());
            for sample in chunk_start..chunk_end {
                let relative = sample - chunk.start_sample();
                for (requested, &channel) in channels.iter().enumerate() {
                    let value = chunk
                        .packed_level(relative, channel)
                        .expect("validated committed chunk contains every requested sample");
                    match current[requested] {
                        None => {
                            sampled[requested].initial = value;
                            current[requested] = Some(value);
                        }
                        Some(previous) if previous != value => {
                            sampled[requested]
                                .transitions
                                .push(CaptureTransition { sample, value });
                            current[requested] = Some(value);
                        }
                        Some(_) => {}
                    }
                }
            }
        }

        Ok(CaptureSampledWindow {
            start_sample,
            end_sample,
            sample_step: 1,
            channels: sampled,
        })
    }

    fn sequence_containing(
        &mut self,
        sample: u64,
        committed_chunks: u64,
    ) -> CaptureStoreResult<u64> {
        let mut lo = 0_u64;
        let mut hi = committed_chunks;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let record = read_commit_record_at(
                &mut self.commit_file,
                mid,
                self.commit_format_version,
            )?;
            if record.start_sample.saturating_add(record.sample_count) <= sample {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo >= committed_chunks {
            return Err(CaptureStoreError::Corrupt(format!(
                "no committed chunk contains sample {sample}"
            )));
        }
        let record = read_commit_record_at(
            &mut self.commit_file,
            lo,
            self.commit_format_version,
        )?;
        if sample < record.start_sample
            || sample >= record.start_sample.saturating_add(record.sample_count)
        {
            return Err(CaptureStoreError::Corrupt(format!(
                "committed chunks do not cover sample {sample}"
            )));
        }
        Ok(lo)
    }
}

fn store_as_capture_error(error: CaptureStoreError) -> Error {
    match error {
        CaptureStoreError::Io(error) => Error::Io(error),
        error => Error::ParseError(error.to_string()),
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
    bytes.extend_from_slice(&COMMIT_FORMAT_VERSION.to_le_bytes());
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
) -> CaptureStoreResult<u16> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = [0_u8; COMMIT_HEADER_SIZE as usize];
    file.read_exact(&mut bytes)?;
    let format_version = get_u16(&bytes, 8)?;
    if &bytes[0..8] != COMMIT_MAGIC
        || !matches!(
            format_version,
            LEGACY_COMMIT_FORMAT_VERSION | COMMIT_FORMAT_VERSION
        )
        || get_u16(&bytes, 10)? != COMMIT_HEADER_SIZE
        || get_u16(&bytes, 12)? != COMMIT_RECORD_SIZE
        || get_u128(&bytes, 16)? != expected_session.get()
    {
        return Err(CaptureStoreError::Corrupt(
            "commit header is incompatible with the session".into(),
        ));
    }
    Ok(format_version)
}

fn encode_commit_record(record: CommitRecord, output: &mut Vec<u8>) {
    output.extend_from_slice(&commit_record_prefix(record));
    output.extend_from_slice(&record.checksum.to_le_bytes());
    output.extend_from_slice(&[0_u8; 2]);
}

fn commit_record_prefix(record: CommitRecord) -> [u8; 42] {
    let mut bytes = [0_u8; 42];
    bytes[0..8].copy_from_slice(&record.sequence.to_le_bytes());
    bytes[8..16].copy_from_slice(&record.start_sample.to_le_bytes());
    bytes[16..24].copy_from_slice(&record.sample_count.to_le_bytes());
    bytes[24..32].copy_from_slice(&record.data_offset.to_le_bytes());
    bytes[32..40].copy_from_slice(&record.data_len.to_le_bytes());
    bytes[40] = record.bit_offset;
    bytes[41] = record.encoding;
    bytes
}

fn commit_record_checksum(record: CommitRecord, data: &[u8]) -> u32 {
    let prefix = commit_record_prefix(record);
    crate::derived_word_store::crc32c::checksum_parts(&[&prefix, data])
}

fn validate_record_checksum(
    commit_format_version: u16,
    record: CommitRecord,
    data: &[u8],
) -> CaptureStoreResult<()> {
    if commit_format_version == LEGACY_COMMIT_FORMAT_VERSION {
        return Ok(());
    }
    let actual = commit_record_checksum(record, data);
    if actual != record.checksum {
        return Err(CaptureStoreError::Corrupt(format!(
            "capture chunk {} checksum mismatch",
            record.sequence
        )));
    }
    Ok(())
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
        checksum: get_u32(bytes, 42)?,
    })
}

fn read_commit_record_at(
    commit_file: &mut File,
    sequence: u64,
    commit_format_version: u16,
) -> CaptureStoreResult<CommitRecord> {
    let offset = u64::from(COMMIT_HEADER_SIZE)
        .checked_add(
            sequence
                .checked_mul(u64::from(COMMIT_RECORD_SIZE))
                .ok_or_else(|| CaptureStoreError::Corrupt("commit offset overflow".into()))?,
        )
        .ok_or_else(|| CaptureStoreError::Corrupt("commit offset overflow".into()))?;
    commit_file.seek(SeekFrom::Start(offset))?;
    let mut bytes = [0_u8; COMMIT_RECORD_SIZE as usize];
    commit_file.read_exact(&mut bytes)?;
    let record = decode_commit_record(&bytes)?;
    if commit_format_version == LEGACY_COMMIT_FORMAT_VERSION && record.checksum != 0 {
        return Err(CaptureStoreError::Corrupt(format!(
            "legacy commit slot {sequence} has non-zero reserved bytes"
        )));
    }
    if record.sequence != sequence {
        return Err(CaptureStoreError::Corrupt(format!(
            "commit slot {sequence} contains sequence {}",
            record.sequence
        )));
    }
    Ok(record)
}

fn read_record_chunk(
    shared: &SharedStore,
    data_file: &mut File,
    commit_format_version: u16,
    record: CommitRecord,
) -> CaptureStoreResult<CaptureChunk> {
    if record.encoding != PACKED_LSB_FIRST_ENCODING {
        return Err(CaptureStoreError::Corrupt(format!(
            "unsupported chunk encoding {}",
            record.encoding
        )));
    }
    let data_len = usize::try_from(record.data_len)
        .map_err(|_| CaptureStoreError::Corrupt("chunk data length is too large".into()))?;
    let mut data = vec![0_u8; data_len];
    data_file.seek(SeekFrom::Start(record.data_offset))?;
    data_file.read_exact(&mut data)?;
    validate_record_checksum(commit_format_version, record, &data)?;
    CaptureChunk::packed_lsb_first(
        shared.descriptor.session_id(),
        record.sequence,
        record.start_sample,
        record.sample_count,
        shared.descriptor.channel_table(),
        data,
        record.bit_offset,
    )
    .map_err(|error| CaptureStoreError::Corrupt(error.to_string()))
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

fn write_session_plan(directory: &Path, plan: &CaptureSessionPlan) -> CaptureStoreResult<()> {
    let mut bytes = serde_json::to_vec_pretty(plan).map_err(|error| {
        CaptureStoreError::InvalidConfig(format!("capture session plan cannot be encoded: {error}"))
    })?;
    bytes.push(b'\n');
    let temp_path = directory.join(PLAN_TEMP_FILE_NAME);
    let final_path = directory.join(PLAN_FILE_NAME);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)?;
    file.write_all(&bytes)?;
    file.sync_data()?;
    drop(file);
    fs::rename(temp_path, final_path)?;
    Ok(())
}

fn read_session_plan(directory: &Path) -> CaptureStoreResult<Option<CaptureSessionPlan>> {
    let path = directory.join(PLAN_FILE_NAME);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            CaptureStoreError::Corrupt(format!(
                "capture session plan {} is invalid: {error}",
                path.display()
            ))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedSessionMetadata {
    format_version: u16,
    session_id: String,
    channels: Vec<String>,
    #[serde(default)]
    timeline: Option<PersistedCaptureTimelineMetadata>,
    outcome: CaptureSessionOutcome,
    created_unix_ns: u64,
    accessed_unix_ns: u64,
    recording_origin: Option<u64>,
    retained_start_sample: u64,
    kept: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedCaptureTimelineMetadata {
    sample_rate_hz: f64,
    channel_names: Vec<String>,
    trigger_sample: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedReclamation {
    format_version: u16,
    committed_chunks: u64,
    committed_samples: u64,
    committed_data_bytes: u64,
    metadata: PersistedSessionMetadata,
    plan: Option<CaptureSessionPlan>,
}

impl PersistedSessionMetadata {
    fn from_metadata(metadata: &CaptureSessionMetadata) -> Self {
        Self {
            format_version: SESSION_FORMAT_VERSION,
            session_id: format!("{:032x}", metadata.descriptor.session_id().get()),
            channels: metadata
                .descriptor
                .channels()
                .iter()
                .map(ToString::to_string)
                .collect(),
            timeline: metadata.timeline.as_ref().map(|timeline| {
                PersistedCaptureTimelineMetadata {
                    sample_rate_hz: timeline.sample_rate_hz(),
                    channel_names: timeline.channel_names().to_vec(),
                    trigger_sample: timeline.trigger_sample(),
                }
            }),
            outcome: metadata.outcome,
            created_unix_ns: metadata.created_unix_ns,
            accessed_unix_ns: metadata.accessed_unix_ns,
            recording_origin: metadata.recording_origin,
            retained_start_sample: metadata.retained_start_sample,
            kept: metadata.kept,
        }
    }

    fn into_metadata(self) -> CaptureStoreResult<CaptureSessionMetadata> {
        if self.format_version != 1 && self.format_version != SESSION_FORMAT_VERSION {
            return Err(CaptureStoreError::Corrupt(format!(
                "unsupported capture session metadata version {}",
                self.format_version
            )));
        }
        let session_id = u128::from_str_radix(&self.session_id, 16).map_err(|_| {
            CaptureStoreError::Corrupt("capture session ID is not hexadecimal".into())
        })?;
        let descriptor = CaptureStoreDescriptor::new(
            CaptureSessionId::new(session_id),
            self.channels
                .into_iter()
                .map(crate::CaptureChannelId::new)
                .collect::<Vec<_>>(),
        )?;
        let timeline = self
            .timeline
            .map(|timeline| {
                let mut decoded = CaptureTimelineMetadata::new(
                    timeline.sample_rate_hz,
                    timeline.channel_names,
                )?;
                if decoded.channel_names().len() != descriptor.channels().len() {
                    return Err(CaptureStoreError::Corrupt(format!(
                        "capture timeline has {} channel names for {} channels",
                        decoded.channel_names().len(),
                        descriptor.channels().len()
                    )));
                }
                decoded.set_trigger_sample(timeline.trigger_sample);
                Ok(decoded)
            })
            .transpose()?;
        Ok(CaptureSessionMetadata {
            descriptor,
            timeline,
            outcome: self.outcome,
            created_unix_ns: self.created_unix_ns,
            accessed_unix_ns: self.accessed_unix_ns,
            recording_origin: self.recording_origin,
            retained_start_sample: self.retained_start_sample,
            kept: self.kept,
        })
    }
}

fn write_session_metadata(
    directory: &Path,
    metadata: &CaptureSessionMetadata,
) -> CaptureStoreResult<()> {
    let mut bytes = serde_json::to_vec_pretty(&PersistedSessionMetadata::from_metadata(metadata))
        .map_err(|error| {
            CaptureStoreError::InvalidConfig(format!(
                "capture session metadata cannot be encoded: {error}"
            ))
        })?;
    bytes.push(b'\n');
    let temp_path = directory.join(SESSION_TEMP_FILE_NAME);
    let final_path = directory.join(SESSION_FILE_NAME);
    remove_if_exists(&temp_path)?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)?;
    file.write_all(&bytes)?;
    file.sync_data()?;
    drop(file);
    fs::rename(temp_path, final_path)?;
    Ok(())
}

fn read_session_metadata(directory: &Path) -> CaptureStoreResult<Option<CaptureSessionMetadata>> {
    let path = directory.join(SESSION_FILE_NAME);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<PersistedSessionMetadata>(&bytes)
            .map_err(|error| {
                CaptureStoreError::Corrupt(format!(
                    "capture session metadata {} is invalid: {error}",
                    path.display()
                ))
            })?
            .into_metadata()
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_reclamation(
    directory: &Path,
    transaction: &PersistedReclamation,
) -> CaptureStoreResult<()> {
    let mut bytes = serde_json::to_vec_pretty(transaction).map_err(|error| {
        CaptureStoreError::InvalidConfig(format!(
            "capture reclamation transaction cannot be encoded: {error}"
        ))
    })?;
    bytes.push(b'\n');
    let temp_path = directory.join(RECLAIM_TEMP_FILE_NAME);
    let final_path = directory.join(RECLAIM_FILE_NAME);
    remove_if_exists(&temp_path)?;
    let mut file = create_new_file(&temp_path)?;
    file.write_all(&bytes)?;
    file.sync_data()?;
    drop(file);
    fs::rename(temp_path, final_path)?;
    Ok(())
}

fn finish_pending_reclamation(directory: &Path) -> CaptureStoreResult<()> {
    finish_pending_reclamation_with_hook(directory, &mut |_| Ok(()))
}

fn finish_pending_reclamation_with_hook(
    directory: &Path,
    after_step: &mut impl FnMut(ReclamationDurableStep) -> CaptureStoreResult<()>,
) -> CaptureStoreResult<()> {
    let transaction_path = directory.join(RECLAIM_FILE_NAME);
    let bytes = match fs::read(&transaction_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let transaction = serde_json::from_slice::<PersistedReclamation>(&bytes).map_err(|error| {
        CaptureStoreError::Corrupt(format!(
            "capture reclamation transaction is invalid: {error}"
        ))
    })?;
    if transaction.format_version != RECLAIM_FORMAT_VERSION {
        return Err(CaptureStoreError::Corrupt(format!(
            "unsupported capture reclamation version {}",
            transaction.format_version
        )));
    }
    install_reclaimed_file(
        directory,
        DATA_FILE_NAME,
        RECLAIM_DATA_FILE_NAME,
        RECLAIM_OLD_DATA_FILE_NAME,
        ReclamationDurableStep::DataBackedUp,
        ReclamationDurableStep::DataInstalled,
        after_step,
    )?;
    install_reclaimed_file(
        directory,
        COMMIT_FILE_NAME,
        RECLAIM_COMMIT_FILE_NAME,
        RECLAIM_OLD_COMMIT_FILE_NAME,
        ReclamationDurableStep::CommitsBackedUp,
        ReclamationDurableStep::CommitsInstalled,
        after_step,
    )?;
    let metadata = transaction.metadata.into_metadata()?;
    let manifest = CaptureStoreManifest {
        descriptor: metadata.descriptor.clone(),
        committed_chunks: transaction.committed_chunks,
        committed_samples: transaction.committed_samples,
        committed_data_bytes: transaction.committed_data_bytes,
    };
    if let Some(plan) = &transaction.plan {
        remove_if_exists(&directory.join(PLAN_TEMP_FILE_NAME))?;
        write_session_plan(directory, plan)?;
        after_step(ReclamationDurableStep::PlanInstalled)?;
    }
    write_session_metadata(directory, &metadata)?;
    after_step(ReclamationDurableStep::MetadataInstalled)?;
    write_manifest(directory, &manifest)?;
    after_step(ReclamationDurableStep::ManifestInstalled)?;
    remove_if_exists(&directory.join(RECLAIM_OLD_DATA_FILE_NAME))?;
    remove_if_exists(&directory.join(RECLAIM_OLD_COMMIT_FILE_NAME))?;
    after_step(ReclamationDurableStep::BackupsRemoved)?;
    remove_if_exists(&transaction_path)?;
    after_step(ReclamationDurableStep::TransactionRemoved)?;
    Ok(())
}

fn install_reclaimed_file(
    directory: &Path,
    current_name: &str,
    replacement_name: &str,
    old_name: &str,
    backup_step: ReclamationDurableStep,
    install_step: ReclamationDurableStep,
    after_step: &mut impl FnMut(ReclamationDurableStep) -> CaptureStoreResult<()>,
) -> CaptureStoreResult<()> {
    let current = directory.join(current_name);
    let replacement = directory.join(replacement_name);
    let old = directory.join(old_name);
    if replacement.is_file() {
        if current.is_file() {
            remove_if_exists(&old)?;
            fs::rename(&current, &old)?;
            after_step(backup_step)?;
        }
        fs::rename(&replacement, &current)?;
        after_step(install_step)?;
    } else if !current.is_file() {
        return Err(CaptureStoreError::Corrupt(format!(
            "reclamation lost both {current_name} and its replacement"
        )));
    }
    Ok(())
}

fn remove_waveform_summaries(directory: &Path) -> CaptureStoreResult<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("capture.waveform."))
        {
            remove_if_exists(&entry.path())?;
        }
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> CaptureStoreResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn unix_ns() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    )
    .unwrap_or(u64::MAX)
}

fn encode_manifest(manifest: &CaptureStoreManifest) -> CaptureStoreResult<Vec<u8>> {
    let channel_count = u32::try_from(manifest.descriptor.channels().len())
        .map_err(|_| CaptureStoreError::InvalidConfig("too many capture channels".into()))?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MANIFEST_MAGIC);
    bytes.extend_from_slice(&MANIFEST_FORMAT_VERSION.to_le_bytes());
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
        || get_u16(&bytes, 8)? != MANIFEST_FORMAT_VERSION
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
    let _ = validate_commit_header(&mut commit_file, manifest.descriptor.session_id())?;
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
    use std::fs::OpenOptions;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::tempdir;

    use crate::{
        CaptureCapacityEstimate, CaptureChannelId, CaptureChunk, CaptureChunkWriter,
        CaptureFraction, CapturePolicy, CaptureSessionId, CaptureSessionPlan, CaptureStoreCursor,
        CaptureSessionOutcome, CaptureTimelineMetadata, CompletionPolicy, EffectiveCapturePolicy,
        RecordingStart, RetentionPolicy, TriggerPlacement,
    };

    use super::{
        CaptureCursorItem, CaptureStoreDescriptor, CaptureStoreError, NativeCaptureStore,
        NativeCaptureStoreConfig, NativeFinalizedCapture, COMMIT_FILE_NAME, COMMIT_HEADER_SIZE,
        COMMIT_RECORD_SIZE, DATA_FILE_NAME, LEGACY_COMMIT_FORMAT_VERSION, PLAN_FILE_NAME,
        PLAN_TEMP_FILE_NAME, ReclamationDurableStep,
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

    fn session_plan() -> CaptureSessionPlan {
        let policy = CapturePolicy {
            start: RecordingStart::Trigger,
            trigger_placement: Some(TriggerPlacement::Fraction(
                CaptureFraction::from_percent(25).unwrap(),
            )),
            retention_before_origin: RetentionPolicy::RecentDuration(Duration::from_secs(2)),
            retention_after_origin: RetentionPolicy::Everything,
            completion: CompletionPolicy::SamplesAfterOrigin(768),
            trigger_timeout: None,
        };
        CaptureSessionPlan {
            sample_rate_hz: 500_000_000,
            channel_count: 3,
            policy: EffectiveCapturePolicy {
                requested: policy.clone(),
                effective: policy,
            },
            capacity: CaptureCapacityEstimate {
                worst_case_bytes_per_second: 187_500_000,
                finite_capture_bytes: Some(384),
                retained_duration: Some(Duration::from_secs(2)),
                sustainable: Some(true),
                warnings: vec!["capacity estimate test".into()],
            },
        }
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
    fn finalized_store_reopens_legacy_unchecksummed_commit_records() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let (store, mut writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
            temporary.path(),
            descriptor.clone(),
        ))
        .unwrap();
        let expected = chunk(&descriptor, 0, 0, 7);
        writer.append(expected.clone()).unwrap();
        writer.finish().unwrap();
        drop(writer);
        let finalized = store.finalize().unwrap();
        let commit_path = finalized.directory().join(COMMIT_FILE_NAME);
        let mut bytes = std::fs::read(&commit_path).unwrap();
        bytes[8..10].copy_from_slice(&LEGACY_COMMIT_FORMAT_VERSION.to_le_bytes());
        let checksum = usize::from(COMMIT_HEADER_SIZE) + 42;
        bytes[checksum..checksum + 4].fill(0);
        assert_eq!(bytes.len(), usize::from(COMMIT_HEADER_SIZE + COMMIT_RECORD_SIZE));
        std::fs::write(commit_path, bytes).unwrap();

        let reopened = NativeFinalizedCapture::open(finalized.directory()).unwrap();
        let mut cursor = reopened.open_cursor().unwrap();
        assert_eq!(cursor.next().unwrap(), CaptureCursorItem::Chunk(expected));
    }

    #[test]
    fn finalized_store_reopens_atomic_session_plan() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let (store, mut writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
            temporary.path(),
            descriptor,
        ))
        .unwrap();
        writer.finish().unwrap();
        drop(writer);

        let plan = session_plan();
        store.write_session_plan(&plan).unwrap();
        let finalized = store.finalize().unwrap();
        assert_eq!(finalized.session_plan().unwrap(), Some(plan.clone()));

        let reopened = NativeFinalizedCapture::open(finalized.directory()).unwrap();
        assert_eq!(reopened.session_plan().unwrap(), Some(plan));
        assert!(!temporary.path().join(PLAN_TEMP_FILE_NAME).exists());
    }

    #[test]
    fn malformed_session_plan_rejects_reopen() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let (store, mut writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
            temporary.path(),
            descriptor,
        ))
        .unwrap();
        writer.finish().unwrap();
        drop(writer);
        let finalized = store.finalize().unwrap();
        std::fs::write(finalized.directory().join(PLAN_FILE_NAME), b"not json").unwrap();

        assert!(matches!(
            NativeFinalizedCapture::open(finalized.directory()),
            Err(CaptureStoreError::Corrupt(_))
        ));
    }

    #[test]
    fn recovery_discards_data_without_a_durable_commit() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor.clone())
            .with_commit_batch_chunks(2)
            .unwrap();
        let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
        writer.append(chunk(&descriptor, 0, 0, 7)).unwrap();
        std::mem::forget(writer);
        drop(store);

        let (recovered, report) = NativeFinalizedCapture::recover(temporary.path()).unwrap();
        assert!(report.recovered);
        assert!(report.truncated_data_bytes > 0);
        assert_eq!(recovered.manifest().committed_chunks, 0);
        assert_eq!(recovered.manifest().committed_samples, 0);
        assert_eq!(
            recovered.session_metadata().unwrap().unwrap().outcome,
            CaptureSessionOutcome::Incomplete
        );
    }

    #[test]
    fn recovery_preserves_committed_prefix_and_truncates_partial_tails() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor.clone())
            .with_commit_batch_chunks(1)
            .unwrap();
        let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
        let expected = chunk(&descriptor, 0, 0, 7);
        writer.append(expected.clone()).unwrap();
        std::mem::forget(writer);
        drop(store);
        OpenOptions::new()
            .append(true)
            .open(temporary.path().join(COMMIT_FILE_NAME))
            .unwrap()
            .write_all(&[0xaa, 0xbb, 0xcc])
            .unwrap();
        OpenOptions::new()
            .append(true)
            .open(temporary.path().join(DATA_FILE_NAME))
            .unwrap()
            .write_all(&[0xdd, 0xee])
            .unwrap();

        let (recovered, report) = NativeFinalizedCapture::recover(temporary.path()).unwrap();
        assert_eq!(report.truncated_commit_bytes, 3);
        assert_eq!(report.truncated_data_bytes, 2);
        assert_eq!(recovered.manifest().committed_chunks, 1);
        assert_eq!(recovered.manifest().committed_samples, 7);
        let mut cursor = recovered.open_cursor().unwrap();
        assert_eq!(cursor.next().unwrap(), CaptureCursorItem::Chunk(expected));
        assert_eq!(cursor.next().unwrap(), CaptureCursorItem::End);
    }

    #[test]
    fn recovery_rejects_a_checksum_mismatch_in_a_full_commit() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor.clone())
            .with_commit_batch_chunks(1)
            .unwrap();
        let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
        writer.append(chunk(&descriptor, 0, 0, 7)).unwrap();
        std::mem::forget(writer);
        drop(store);
        let data_path = temporary.path().join(DATA_FILE_NAME);
        let mut data = OpenOptions::new()
            .read(true)
            .write(true)
            .open(data_path)
            .unwrap();
        let mut first = [0_u8; 1];
        data.read_exact(&mut first).unwrap();
        data.seek(SeekFrom::Start(0)).unwrap();
        data.write_all(&[first[0] ^ 0x80]).unwrap();
        data.sync_data().unwrap();

        assert!(matches!(
            NativeFinalizedCapture::recover(temporary.path()),
            Err(CaptureStoreError::Corrupt(error)) if error.contains("checksum")
        ));
    }

    #[test]
    fn recovery_rejects_a_commit_whose_data_write_is_not_durable() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor.clone())
            .with_commit_batch_chunks(1)
            .unwrap();
        let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
        writer.append(chunk(&descriptor, 0, 0, 7)).unwrap();
        std::mem::forget(writer);
        drop(store);
        let data_path = temporary.path().join(DATA_FILE_NAME);
        let data = OpenOptions::new().write(true).open(data_path).unwrap();
        let truncated = data.metadata().unwrap().len().saturating_sub(1);
        data.set_len(truncated).unwrap();
        data.sync_data().unwrap();

        assert!(matches!(
            NativeFinalizedCapture::recover(temporary.path()),
            Err(CaptureStoreError::Corrupt(error)) if error.contains("beyond durable data")
        ));
    }

    #[test]
    fn recovery_finishes_a_terminal_session_after_metadata_but_before_manifest() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor.clone())
            .with_commit_batch_chunks(1)
            .unwrap();
        let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
        writer.append(chunk(&descriptor, 0, 0, 7)).unwrap();
        writer.finish().unwrap();
        drop(writer);
        let mut metadata = super::read_session_metadata(temporary.path())
            .unwrap()
            .unwrap();
        metadata.outcome = CaptureSessionOutcome::Stopped;
        metadata.recording_origin = Some(3);
        super::write_session_metadata(temporary.path(), &metadata).unwrap();
        drop(store);

        let (recovered, report) = NativeFinalizedCapture::recover(temporary.path()).unwrap();
        assert!(report.recovered);
        assert_eq!(recovered.manifest().committed_chunks, 1);
        let metadata = recovered.session_metadata().unwrap().unwrap();
        assert_eq!(metadata.outcome, CaptureSessionOutcome::Stopped);
        assert_eq!(metadata.recording_origin, Some(3));
    }

    #[test]
    fn timeline_metadata_is_durable_and_tracks_reclamation() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let (store, mut writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
            temporary.path(),
            descriptor.clone(),
        ))
        .unwrap();
        store
            .write_timeline_metadata(
                CaptureTimelineMetadata::new(
                    500_000_000.0,
                    vec!["Clock".into(), "Data".into(), "Enable".into()],
                )
                .unwrap(),
            )
            .unwrap();
        writer.append(chunk(&descriptor, 0, 0, 7)).unwrap();
        writer.append(chunk(&descriptor, 1, 7, 7)).unwrap();
        writer.finish().unwrap();
        drop(writer);
        let finalized = store
            .finalize_with_details(CaptureSessionOutcome::Complete, Some(9), Some(9))
            .unwrap();
        let timeline = finalized
            .session_metadata()
            .unwrap()
            .unwrap()
            .timeline
            .unwrap();
        assert_eq!(timeline.sample_rate_hz(), 500_000_000.0);
        assert_eq!(timeline.channel_names(), ["Clock", "Data", "Enable"]);
        assert_eq!(timeline.trigger_sample(), Some(9));

        let (reclaimed, report) =
            NativeFinalizedCapture::reclaim_directory_before(temporary.path(), 7).unwrap();
        assert_eq!(report.reclaimed_samples, 7);
        let timeline = reclaimed
            .session_metadata()
            .unwrap()
            .unwrap()
            .timeline
            .unwrap();
        assert_eq!(timeline.trigger_sample(), Some(2));
    }

    #[test]
    fn version_one_session_metadata_without_a_timeline_still_loads() {
        let temporary = tempdir().unwrap();
        let (_store, writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
            temporary.path(),
            descriptor(),
        ))
        .unwrap();
        drop(writer);
        let path = temporary.path().join(super::SESSION_FILE_NAME);
        let mut persisted = serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(&path).unwrap(),
        )
        .unwrap();
        persisted["format_version"] = serde_json::json!(1);
        persisted.as_object_mut().unwrap().remove("timeline");
        std::fs::write(&path, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();

        let metadata = super::read_session_metadata(temporary.path())
            .unwrap()
            .unwrap();
        assert!(metadata.timeline.is_none());
        assert_eq!(metadata.descriptor, descriptor());
    }

    #[test]
    fn recovery_completes_reclamation_after_every_durable_step() {
        let steps = [
            ReclamationDurableStep::TransactionWritten,
            ReclamationDurableStep::DataBackedUp,
            ReclamationDurableStep::DataInstalled,
            ReclamationDurableStep::CommitsBackedUp,
            ReclamationDurableStep::CommitsInstalled,
            ReclamationDurableStep::PlanInstalled,
            ReclamationDurableStep::MetadataInstalled,
            ReclamationDurableStep::ManifestInstalled,
            ReclamationDurableStep::BackupsRemoved,
            ReclamationDurableStep::TransactionRemoved,
        ];
        for interrupted_after in steps {
            let temporary = tempdir().unwrap();
            let descriptor = descriptor();
            let config = NativeCaptureStoreConfig::new(temporary.path(), descriptor.clone())
                .with_commit_batch_chunks(1)
                .unwrap();
            let (store, mut writer) = NativeCaptureStore::create(config).unwrap();
            let mut start = 0;
            for (sequence, samples) in [7_u64, 7, 7].into_iter().enumerate() {
                writer
                    .append(chunk(&descriptor, sequence as u64, start, samples))
                    .unwrap();
                start += samples;
            }
            writer.finish().unwrap();
            drop(writer);
            store.write_session_plan(&session_plan()).unwrap();
            store
                .finalize_with_outcome(CaptureSessionOutcome::Complete, Some(7))
                .unwrap();

            let result = NativeFinalizedCapture::reclaim_directory_before_with_hook(
                temporary.path(),
                7,
                |step| {
                    if step == interrupted_after {
                        Err(std::io::Error::other(format!(
                            "injected interruption after {step:?}"
                        ))
                        .into())
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err(), "missing interruption at {interrupted_after:?}");

            let (recovered, _) = NativeFinalizedCapture::recover(temporary.path()).unwrap();
            assert_eq!(recovered.manifest().committed_chunks, 2);
            assert_eq!(recovered.manifest().committed_samples, 14);
            let metadata = recovered.session_metadata().unwrap().unwrap();
            assert_eq!(metadata.retained_start_sample, 7);
            assert_eq!(metadata.recording_origin, Some(0));
            let mut cursor = recovered.open_cursor().unwrap();
            for (sequence, start) in [(0, 0), (1, 7)] {
                let CaptureCursorItem::Chunk(actual) = cursor.next().unwrap() else {
                    panic!("missing retained chunk after {interrupted_after:?}");
                };
                assert_eq!(actual.sequence(), sequence);
                assert_eq!(actual.start_sample(), start);
                assert_eq!(actual.sample_count(), 7);
            }
            assert_eq!(cursor.next().unwrap(), CaptureCursorItem::End);
        }
    }

    #[test]
    fn finalized_outcome_and_recording_origin_are_durable() {
        let temporary = tempdir().unwrap();
        let descriptor = descriptor();
        let (store, mut writer) = NativeCaptureStore::create(NativeCaptureStoreConfig::new(
            temporary.path(),
            descriptor,
        ))
        .unwrap();
        writer.finish().unwrap();
        drop(writer);
        let finalized = store
            .finalize_with_outcome(CaptureSessionOutcome::Aborted, Some(123))
            .unwrap();

        let metadata = NativeFinalizedCapture::open(finalized.directory())
            .unwrap()
            .session_metadata()
            .unwrap()
            .unwrap();
        assert_eq!(metadata.outcome, CaptureSessionOutcome::Aborted);
        assert_eq!(metadata.recording_origin, Some(123));
        assert!(metadata.outcome.is_incomplete());
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
        let mut live_cursor = store.open_live_cursor().unwrap();
        writer.append(chunk(&descriptor, 0, 0, 3)).unwrap();

        assert_eq!(store.snapshot().committed_chunks, 0);
        assert_eq!(cursor.next().unwrap(), CaptureCursorItem::Pending);
        assert!(matches!(
            live_cursor.next().unwrap(),
            CaptureCursorItem::Chunk(_)
        ));
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
