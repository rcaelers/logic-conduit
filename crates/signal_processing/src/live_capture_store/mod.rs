//! Platform-neutral contract for an authoritative live-capture store.

use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use crate::{CaptureChannelId, CaptureChunk, CaptureSessionId};

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        #[path = "native.rs"]
        mod native;

        pub use native::{
            NativeCaptureCursor, NativeCaptureStore, NativeCaptureStoreConfig,
            NativeCaptureStoreWriter, NativeFinalizedCapture,
        };
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureStoreDescriptor {
    session_id: CaptureSessionId,
    channels: Arc<[CaptureChannelId]>,
}

impl CaptureStoreDescriptor {
    pub fn new(
        session_id: CaptureSessionId,
        channels: impl Into<Arc<[CaptureChannelId]>>,
    ) -> CaptureStoreResult<Self> {
        let channels = channels.into();
        if channels.is_empty() {
            return Err(CaptureStoreError::InvalidConfig(
                "capture store requires at least one channel".into(),
            ));
        }
        Ok(Self {
            session_id,
            channels,
        })
    }

    pub const fn session_id(&self) -> CaptureSessionId {
        self.session_id
    }

    pub fn channels(&self) -> &[CaptureChannelId] {
        &self.channels
    }

    pub fn channel_table(&self) -> Arc<[CaptureChannelId]> {
        Arc::clone(&self.channels)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureStoreSnapshot {
    pub committed_chunks: u64,
    pub committed_samples: u64,
    pub committed_data_bytes: u64,
    pub writer_open: bool,
    pub writer_failed: bool,
    pub finalized: bool,
    /// Commit records remain on disk; the live store retains none in an in-memory vector.
    pub resident_commit_records: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureStoreManifest {
    pub descriptor: CaptureStoreDescriptor,
    pub committed_chunks: u64,
    pub committed_samples: u64,
    pub committed_data_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureCursorItem {
    Chunk(CaptureChunk),
    Pending,
    End,
}

pub trait CaptureStoreCursor: Send {
    fn next(&mut self) -> CaptureStoreResult<CaptureCursorItem>;
    fn wait_next(&mut self, timeout: Duration) -> CaptureStoreResult<CaptureCursorItem>;
    fn next_sequence(&self) -> u64;
}

pub type CaptureStoreResult<T> = Result<T, CaptureStoreError>;

#[derive(Debug, Error)]
pub enum CaptureStoreError {
    #[error("invalid capture-store configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid capture chunk: {0}")]
    InvalidChunk(String),
    #[error("capture-store writer is still open")]
    WriterStillOpen,
    #[error("capture store is already finalized")]
    AlreadyFinalized,
    #[error("capture store is not finalized")]
    NotFinalized,
    #[error("capture store failed: {0}")]
    WriterFailed(String),
    #[error("corrupt capture store: {0}")]
    Corrupt(String),
    #[error("capture-store I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
