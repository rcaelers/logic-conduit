/// Platform-neutral block sizing knobs. Native storage uses these to encode
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
