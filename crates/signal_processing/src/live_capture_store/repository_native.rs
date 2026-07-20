//! Native ownership and cleanup for captured sessions.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{
    CaptureReclamationReport, CaptureRecoveryReport, CaptureSessionOutcome, CaptureStoreError,
    CaptureStoreResult, NativeFinalizedCapture,
};
use crate::{CaptureRetentionTracker, CaptureSessionId};

#[derive(Clone, Debug)]
pub struct NativeCaptureSessionRepositoryConfig {
    root: PathBuf,
    max_recent_sessions: usize,
    max_total_bytes: u64,
}

impl NativeCaptureSessionRepositoryConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_recent_sessions: 10,
            max_total_bytes: 20 * 1024 * 1024 * 1024,
        }
    }

    pub fn with_limits(
        mut self,
        max_recent_sessions: usize,
        max_total_bytes: u64,
    ) -> CaptureStoreResult<Self> {
        if max_recent_sessions == 0 || max_total_bytes == 0 {
            return Err(CaptureStoreError::InvalidConfig(
                "capture-session limits must be non-zero".into(),
            ));
        }
        self.max_recent_sessions = max_recent_sessions;
        self.max_total_bytes = max_total_bytes;
        Ok(self)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCaptureSessionSummary {
    pub session_id: Option<CaptureSessionId>,
    pub directory: PathBuf,
    pub outcome: CaptureSessionOutcome,
    pub created_unix_ns: u64,
    pub accessed_unix_ns: u64,
    pub committed_samples: u64,
    pub bytes: u64,
    pub kept: bool,
    pub recovery: CaptureRecoveryReport,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureSessionCleanupPlan {
    pub total_sessions: usize,
    pub total_bytes: u64,
    pub over_session_limit: usize,
    pub over_byte_limit: u64,
    /// Oldest unkept, unpinned sessions first. The caller must explicitly discard them.
    pub discard_candidates: Vec<CaptureSessionId>,
}

#[derive(Default)]
struct RepositoryPins {
    sessions: HashMap<CaptureSessionId, usize>,
}

#[derive(Clone)]
pub struct NativeCaptureSessionRepository {
    config: NativeCaptureSessionRepositoryConfig,
    pins: Arc<Mutex<RepositoryPins>>,
}

impl NativeCaptureSessionRepository {
    pub fn new(config: NativeCaptureSessionRepositoryConfig) -> CaptureStoreResult<Self> {
        fs::create_dir_all(config.root())?;
        Ok(Self {
            config,
            pins: Arc::new(Mutex::new(RepositoryPins::default())),
        })
    }

    pub fn root(&self) -> &Path {
        self.config.root()
    }

    pub fn session_directory(&self, session_id: CaptureSessionId) -> PathBuf {
        self.root().join(format!("{:032x}", session_id.get()))
    }

    pub fn reserve(
        &self,
        session_id: CaptureSessionId,
    ) -> CaptureStoreResult<NativeCaptureSessionPin> {
        // Cache roots may be cleared after the application has initialized
        // the repository. Recreate the root at the point of reservation so
        // the next capture does not fail with ENOENT.
        fs::create_dir_all(self.root())?;
        let directory = self.session_directory(session_id);
        fs::create_dir(&directory)?;
        Ok(self.pin_unchecked(session_id, directory))
    }

    pub fn pin(&self, session_id: CaptureSessionId) -> CaptureStoreResult<NativeCaptureSessionPin> {
        let directory = self.session_directory(session_id);
        if !directory.is_dir() {
            return Err(CaptureStoreError::SessionNotFound(session_id));
        }
        Ok(self.pin_unchecked(session_id, directory))
    }

    fn pin_unchecked(
        &self,
        session_id: CaptureSessionId,
        directory: PathBuf,
    ) -> NativeCaptureSessionPin {
        let mut pins = self.pins.lock().unwrap_or_else(|error| error.into_inner());
        *pins.sessions.entry(session_id).or_default() += 1;
        drop(pins);
        NativeCaptureSessionPin {
            session_id,
            directory,
            pins: Arc::clone(&self.pins),
        }
    }

    pub fn is_pinned(&self, session_id: CaptureSessionId) -> bool {
        self.pins
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sessions
            .get(&session_id)
            .is_some_and(|pins| *pins != 0)
    }

    pub fn scan(&self) -> CaptureStoreResult<Vec<NativeCaptureSessionSummary>> {
        let mut summaries = Vec::new();
        for entry in fs::read_dir(self.root())? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let directory = entry.path();
            match NativeFinalizedCapture::recover(&directory) {
                Ok((capture, recovery)) => {
                    let manifest = capture.manifest();
                    let metadata = capture.session_metadata()?;
                    let (outcome, created_unix_ns, accessed_unix_ns, kept) = metadata.map_or(
                        (CaptureSessionOutcome::Complete, 0, 0, false),
                        |metadata| {
                            (
                                metadata.outcome,
                                metadata.created_unix_ns,
                                metadata.accessed_unix_ns,
                                metadata.kept,
                            )
                        },
                    );
                    summaries.push(NativeCaptureSessionSummary {
                        session_id: Some(manifest.descriptor.session_id()),
                        directory: directory.clone(),
                        outcome,
                        created_unix_ns,
                        accessed_unix_ns,
                        committed_samples: manifest.committed_samples,
                        bytes: directory_size(&directory),
                        kept,
                        recovery,
                        error: None,
                    });
                }
                Err(error) => summaries.push(NativeCaptureSessionSummary {
                    session_id: parse_session_directory_id(&entry.file_name()),
                    directory: directory.clone(),
                    outcome: CaptureSessionOutcome::Corrupt,
                    created_unix_ns: 0,
                    accessed_unix_ns: 0,
                    committed_samples: 0,
                    bytes: directory_size(&directory),
                    kept: false,
                    recovery: CaptureRecoveryReport::default(),
                    error: Some(error.to_string()),
                }),
            }
        }
        summaries.sort_by(|left, right| {
            right
                .accessed_unix_ns
                .cmp(&left.accessed_unix_ns)
                .then_with(|| right.created_unix_ns.cmp(&left.created_unix_ns))
        });
        Ok(summaries)
    }

    pub fn open(
        &self,
        session_id: CaptureSessionId,
    ) -> CaptureStoreResult<(NativeFinalizedCapture, NativeCaptureSessionPin)> {
        let pin = self.pin(session_id)?;
        let (capture, _) = NativeFinalizedCapture::recover(pin.directory())?;
        capture.touch()?;
        Ok((capture, pin))
    }

    pub fn set_kept(&self, session_id: CaptureSessionId, kept: bool) -> CaptureStoreResult<()> {
        let (capture, _pin) = self.open(session_id)?;
        capture.set_kept(kept)
    }

    pub fn reclaim_to_policy(
        &self,
        session_id: CaptureSessionId,
    ) -> CaptureStoreResult<CaptureReclamationReport> {
        let pins = self.pins.lock().unwrap_or_else(|error| error.into_inner());
        if pins
            .sessions
            .get(&session_id)
            .is_some_and(|count| *count != 0)
        {
            return Err(CaptureStoreError::SessionPinned(session_id));
        }
        let directory = self.session_directory(session_id);
        if !directory.is_dir() {
            return Err(CaptureStoreError::SessionNotFound(session_id));
        }
        let (capture, _) = NativeFinalizedCapture::recover(&directory)?;
        let manifest = capture.manifest();
        let metadata = capture.session_metadata()?.ok_or_else(|| {
            CaptureStoreError::InvalidConfig(
                "legacy capture sessions have no retention metadata".into(),
            )
        })?;
        let plan = capture.session_plan()?.ok_or_else(|| {
            CaptureStoreError::InvalidConfig(
                "capture session has no negotiated retention policy".into(),
            )
        })?;
        let tracker = CaptureRetentionTracker::new(
            plan.sample_rate_hz,
            plan.policy.effective.retention_before_origin,
            plan.policy.effective.retention_after_origin,
        )
        .map_err(|error| CaptureStoreError::InvalidConfig(error.to_string()))?;
        let safe = tracker.safe_reclaim_before(
            manifest.committed_samples,
            manifest.committed_data_bytes,
            metadata.recording_origin,
        );
        let (_, report) = NativeFinalizedCapture::reclaim_directory_before(&directory, safe)?;
        drop(pins);
        Ok(report)
    }

    pub fn discard(&self, session_id: CaptureSessionId) -> CaptureStoreResult<()> {
        if self.is_pinned(session_id) {
            return Err(CaptureStoreError::SessionPinned(session_id));
        }
        let directory = self.session_directory(session_id);
        if !directory.is_dir() {
            return Err(CaptureStoreError::SessionNotFound(session_id));
        }
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    pub fn cleanup_plan(&self) -> CaptureStoreResult<CaptureSessionCleanupPlan> {
        let summaries = self.scan()?;
        Ok(self.cleanup_plan_for(&summaries))
    }

    pub fn scan_with_cleanup_plan(
        &self,
    ) -> CaptureStoreResult<(Vec<NativeCaptureSessionSummary>, CaptureSessionCleanupPlan)> {
        let summaries = self.scan()?;
        let plan = self.cleanup_plan_for(&summaries);
        Ok((summaries, plan))
    }

    fn cleanup_plan_for(
        &self,
        summaries: &[NativeCaptureSessionSummary],
    ) -> CaptureSessionCleanupPlan {
        let total_sessions = summaries.len();
        let total_bytes = summaries
            .iter()
            .map(|summary| summary.bytes)
            .fold(0_u64, u64::saturating_add);
        let mut candidates = summaries
            .iter()
            .filter_map(|summary| {
                let id = summary.session_id?;
                (!summary.kept && !self.is_pinned(id)).then_some((
                    summary.accessed_unix_ns,
                    id,
                    summary.bytes,
                ))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| candidate.0);
        let mut remaining_sessions = total_sessions;
        let mut remaining_bytes = total_bytes;
        let mut discard_candidates = Vec::new();
        for (_, id, bytes) in candidates {
            if remaining_sessions <= self.config.max_recent_sessions
                && remaining_bytes <= self.config.max_total_bytes
            {
                break;
            }
            discard_candidates.push(id);
            remaining_sessions = remaining_sessions.saturating_sub(1);
            remaining_bytes = remaining_bytes.saturating_sub(bytes);
        }
        CaptureSessionCleanupPlan {
            total_sessions,
            total_bytes,
            over_session_limit: total_sessions.saturating_sub(self.config.max_recent_sessions),
            over_byte_limit: total_bytes.saturating_sub(self.config.max_total_bytes),
            discard_candidates,
        }
    }
}

pub struct NativeCaptureSessionPin {
    session_id: CaptureSessionId,
    directory: PathBuf,
    pins: Arc<Mutex<RepositoryPins>>,
}

impl NativeCaptureSessionPin {
    pub const fn session_id(&self) -> CaptureSessionId {
        self.session_id
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

impl Drop for NativeCaptureSessionPin {
    fn drop(&mut self) {
        let mut pins = self.pins.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(count) = pins.sessions.get_mut(&self.session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                pins.sessions.remove(&self.session_id);
            }
        }
    }
}

fn parse_session_directory_id(name: &std::ffi::OsStr) -> Option<CaptureSessionId> {
    let value = name.to_str()?;
    (value.len() == 32)
        .then(|| u128::from_str_radix(value, 16).ok())
        .flatten()
        .map(CaptureSessionId::new)
}

fn directory_size(directory: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(directory) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            entry.metadata().map_or(0, |metadata| {
                if metadata.is_dir() {
                    directory_size(&entry.path())
                } else {
                    metadata.len()
                }
            })
        })
        .fold(0_u64, u64::saturating_add)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{NativeCaptureSessionRepository, NativeCaptureSessionRepositoryConfig};
    use crate::{
        CaptureChannelId, CaptureChunk, CaptureChunkWriter, CaptureCursorItem, CapturePolicy,
        CaptureSessionId, CaptureSessionOutcome, CaptureSessionPlan, CaptureStoreCursor,
        CaptureStoreDescriptor, CompletionPolicy, EffectiveCapturePolicy, NativeCaptureStore,
        NativeCaptureStoreConfig, RecordingStart, RetentionPolicy,
    };

    fn finalized_session(
        repository: &NativeCaptureSessionRepository,
        id: u128,
    ) -> CaptureSessionId {
        let id = CaptureSessionId::new(id);
        let pin = repository.reserve(id).unwrap();
        let descriptor =
            CaptureStoreDescriptor::new(id, vec![CaptureChannelId::new(format!("bank:{id}"))])
                .unwrap();
        let (store, mut writer) =
            NativeCaptureStore::create(NativeCaptureStoreConfig::new(pin.directory(), descriptor))
                .unwrap();
        crate::CaptureChunkWriter::finish(&mut writer).unwrap();
        drop(writer);
        store
            .finalize_with_outcome(CaptureSessionOutcome::Complete, None)
            .unwrap();
        drop(pin);
        id
    }

    #[test]
    fn pinned_session_cannot_be_discarded() {
        let temporary = tempdir().unwrap();
        let repository = NativeCaptureSessionRepository::new(
            NativeCaptureSessionRepositoryConfig::new(temporary.path()),
        )
        .unwrap();
        let id = finalized_session(&repository, 1);
        let (_capture, pin) = repository.open(id).unwrap();

        assert!(matches!(
            repository.discard(id),
            Err(crate::CaptureStoreError::SessionPinned(actual)) if actual == id
        ));
        drop(pin);
        repository.discard(id).unwrap();
        assert!(!repository.session_directory(id).exists());
    }

    #[test]
    fn reserve_recreates_a_cache_root_removed_after_initialization() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("captures");
        let repository =
            NativeCaptureSessionRepository::new(NativeCaptureSessionRepositoryConfig::new(&root))
                .unwrap();
        fs::remove_dir(&root).unwrap();

        let session = CaptureSessionId::new(42);
        let pin = repository.reserve(session).unwrap();

        assert!(pin.directory().is_dir());
    }

    #[test]
    fn cleanup_requires_explicit_decisions_and_skips_kept_or_pinned_sessions() {
        let temporary = tempdir().unwrap();
        let repository = NativeCaptureSessionRepository::new(
            NativeCaptureSessionRepositoryConfig::new(temporary.path())
                .with_limits(1, u64::MAX)
                .unwrap(),
        )
        .unwrap();
        let first = finalized_session(&repository, 1);
        let second = finalized_session(&repository, 2);
        let third = finalized_session(&repository, 3);
        repository.set_kept(first, true).unwrap();
        let pinned = repository.pin(second).unwrap();

        let plan = repository.cleanup_plan().unwrap();
        assert_eq!(plan.total_sessions, 3);
        assert_eq!(plan.over_session_limit, 2);
        assert_eq!(plan.discard_candidates, vec![third]);
        assert!(repository.session_directory(third).is_dir());
        drop(pinned);
    }

    #[test]
    fn scan_recovers_interrupted_sessions_and_keeps_them_visible() {
        let temporary = tempdir().unwrap();
        let repository = NativeCaptureSessionRepository::new(
            NativeCaptureSessionRepositoryConfig::new(temporary.path()),
        )
        .unwrap();
        let id = CaptureSessionId::new(4);
        let pin = repository.reserve(id).unwrap();
        let descriptor =
            CaptureStoreDescriptor::new(id, vec![CaptureChannelId::new("physical:7")]).unwrap();
        let (_store, writer) =
            NativeCaptureStore::create(NativeCaptureStoreConfig::new(pin.directory(), descriptor))
                .unwrap();
        std::mem::forget(writer);
        drop(pin);

        let summaries = repository.scan().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, Some(id));
        assert_eq!(summaries[0].outcome, CaptureSessionOutcome::Incomplete);
        assert!(summaries[0].recovery.recovered);
    }

    #[test]
    fn corrupt_session_remains_visible_until_explicitly_discarded() {
        let temporary = tempdir().unwrap();
        let repository = NativeCaptureSessionRepository::new(
            NativeCaptureSessionRepositoryConfig::new(temporary.path()),
        )
        .unwrap();
        let id = CaptureSessionId::new(0xfeed);
        let directory = repository.session_directory(id);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("capture.session.json"), b"not json").unwrap();

        let summaries = repository.scan().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, Some(id));
        assert_eq!(summaries[0].outcome, CaptureSessionOutcome::Corrupt);
        assert!(summaries[0].error.is_some());
        assert!(directory.is_dir());

        repository.discard(id).unwrap();
        assert!(!directory.exists());
    }

    #[test]
    fn bounded_reclamation_compacts_unpinned_session_and_rebases_chunks() {
        let temporary = tempdir().unwrap();
        let repository = NativeCaptureSessionRepository::new(
            NativeCaptureSessionRepositoryConfig::new(temporary.path()),
        )
        .unwrap();
        let id = CaptureSessionId::new(5);
        let pin = repository.reserve(id).unwrap();
        let channels = vec![CaptureChannelId::new("physical:3")];
        let descriptor = CaptureStoreDescriptor::new(id, channels.clone()).unwrap();
        let (store, mut writer) = NativeCaptureStore::create(
            NativeCaptureStoreConfig::new(pin.directory(), descriptor)
                .with_commit_batch_chunks(1)
                .unwrap(),
        )
        .unwrap();
        for sequence in 0..5_u64 {
            writer
                .append(
                    CaptureChunk::packed_lsb_first(
                        id,
                        sequence,
                        sequence * 1_000,
                        1_000,
                        channels.clone(),
                        vec![sequence as u8; 125],
                        0,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        writer.finish().unwrap();
        drop(writer);
        let policy = CapturePolicy {
            start: RecordingStart::Immediate,
            trigger_placement: None,
            retention_before_origin: RetentionPolicy::Everything,
            retention_after_origin: RetentionPolicy::RecentDuration(Duration::from_secs(2)),
            completion: CompletionPolicy::SamplesAfterOrigin(5_000),
            trigger_timeout: None,
        };
        store
            .write_session_plan(&CaptureSessionPlan {
                sample_rate_hz: 1_000,
                channel_count: 1,
                capture_window_samples: Some(5_000),
                policy: EffectiveCapturePolicy {
                    requested: policy.clone(),
                    effective: policy,
                },
            })
            .unwrap();
        store
            .finalize_with_outcome(CaptureSessionOutcome::Complete, Some(0))
            .unwrap();

        assert!(matches!(
            repository.reclaim_to_policy(id),
            Err(crate::CaptureStoreError::SessionPinned(actual)) if actual == id
        ));
        drop(pin);
        let report = repository.reclaim_to_policy(id).unwrap();
        assert_eq!(report.reclaimed_chunks, 3);
        assert_eq!(report.reclaimed_samples, 3_000);
        assert_eq!(report.reclaimed_data_bytes, 375);

        let (capture, _pin) = repository.open(id).unwrap();
        assert_eq!(capture.manifest().committed_chunks, 2);
        assert_eq!(capture.manifest().committed_samples, 2_000);
        assert_eq!(
            capture
                .session_metadata()
                .unwrap()
                .unwrap()
                .retained_start_sample,
            3_000
        );
        let mut cursor = capture.open_cursor().unwrap();
        let CaptureCursorItem::Chunk(first) = cursor.next().unwrap() else {
            panic!("missing first retained chunk");
        };
        assert_eq!(first.sequence(), 0);
        assert_eq!(first.start_sample(), 0);
        assert_eq!(first.sample_count(), 1_000);
    }
}
