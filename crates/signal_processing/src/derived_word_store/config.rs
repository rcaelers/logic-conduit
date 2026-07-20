/// Platform-neutral block-sizing knobs. Native storage uses these to encode
/// file blocks; the wasm backend retains them so one compiled configuration
/// has the same shape on every target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockCodecConfig {
    pub max_words: usize,
    pub restart_interval: usize,
    pub max_payload_bytes: usize,
    pub max_inter_word_gap_ns: u64,
    pub max_timestamp_span_ns: u64,
}

impl Default for BlockCodecConfig {
    fn default() -> Self {
        Self {
            max_words: 32_768,
            restart_interval: 512,
            max_payload_bytes: 1024 * 1024,
            max_inter_word_gap_ns: 1_000_000,
            max_timestamp_span_ns: u64::MAX,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentStoreConfig {
    pub directory: PathBuf,
    pub cache_key: [u8; 32],
    pub max_cache_bytes: u64,
}

impl PersistentStoreConfig {
    pub fn new(directory: impl Into<PathBuf>, cache_key: [u8; 32]) -> Self {
        Self {
            directory: directory.into(),
            cache_key,
            max_cache_bytes: DEFAULT_MAX_PERSISTENT_CACHE_BYTES,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveStoreConfig {
    pub directory: PathBuf,
    pub cache_key_prefix: [u8; 16],
    pub block: BlockCodecConfig,
    pub hot_tail_publish_words: usize,
    pub hot_tail_publish_interval: Duration,
    pub persistence: Option<PersistentStoreConfig>,
}

impl Default for LiveStoreConfig {
    fn default() -> Self {
        Self {
            directory: super::platform::default_working_directory(),
            cache_key_prefix: [0; 16],
            block: BlockCodecConfig::default(),
            hot_tail_publish_words: DEFAULT_HOT_TAIL_PUBLISH_WORDS,
            hot_tail_publish_interval: DEFAULT_HOT_TAIL_PUBLISH_INTERVAL,
            persistence: None,
        }
    }
}
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_HOT_TAIL_PUBLISH_WORDS: usize = 16_384;
const DEFAULT_HOT_TAIL_PUBLISH_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_MAX_PERSISTENT_CACHE_BYTES: u64 = 50 * 1024 * 1024 * 1024;
