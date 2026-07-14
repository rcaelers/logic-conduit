use crate::runtime::events::Annotation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnotationStoreMetadata {
    pub generation: u64,
    pub total_word_count: u64,
    pub first_timestamp_ns: Option<u64>,
    pub last_timestamp_ns: Option<u64>,
    /// Greatest explicit word end, or word start for instantaneous words.
    pub extent_end_ns: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactAnnotationWindow {
    pub annotations: Vec<Annotation>,
    pub complete: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordPresenceBucket {
    pub start_ns: u64,
    pub end_ns: u64,
    pub word_count: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AnnotationQueryError {
    #[error("invalid annotation query window: start {start_ns} ns is after end {end_ns} ns")]
    InvalidWindow { start_ns: u64, end_ns: u64 },

    #[error("annotation query word limit must be greater than zero")]
    ZeroWordLimit,

    #[error("annotation presence bucket count must be greater than zero")]
    ZeroBucketLimit,

    #[error("annotation presence queries are not implemented yet")]
    PresenceUnavailable,

    #[error("annotation store query failed: {0}")]
    Store(String),
}

pub type AnnotationQueryResult<T> = std::result::Result<T, AnnotationQueryError>;

/// Viewer-oriented query surface shared by indexed and in-memory word lanes.
pub trait AnnotationQuery: Send + Sync {
    fn metadata(&self) -> AnnotationStoreMetadata;

    fn generation(&self) -> u64 {
        self.metadata().generation
    }

    fn presence_window(
        &self,
        _start_ns: u64,
        _end_ns: u64,
        _target_buckets: usize,
    ) -> AnnotationQueryResult<Vec<WordPresenceBucket>> {
        Err(AnnotationQueryError::PresenceUnavailable)
    }

    fn exact_window(
        &self,
        start_ns: u64,
        end_ns: u64,
        max_words: usize,
    ) -> AnnotationQueryResult<ExactAnnotationWindow>;

    fn nearest_boundary(
        &self,
        timestamp_ns: u64,
        max_distance_ns: u64,
    ) -> AnnotationQueryResult<Option<u64>>;
}
