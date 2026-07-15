/// Errors caused by malformed input or a word stream that cannot be encoded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("word block is empty")]
    EmptyBlock,

    #[error(
        "word timestamp at index {index} moved backwards from {previous_timestamp_ns} ns to {timestamp_ns} ns"
    )]
    OutOfOrder {
        index: usize,
        previous_timestamp_ns: u64,
        timestamp_ns: u64,
    },

    #[error("restart interval must be greater than zero")]
    InvalidRestartInterval,

    #[error("invalid block codec configuration: {0}")]
    InvalidConfiguration(&'static str),

    #[error("word block contains too many records: {0}")]
    TooManyWords(usize),

    #[error("truncated derived-word block")]
    Truncated,

    #[error("unsigned VLQ exceeds 64 bits")]
    VlqOverflow,

    #[error("invalid derived-word format: {0}")]
    InvalidFormat(String),

    #[error("derived-word block checksum mismatch: expected {expected:#010x}, got {actual:#010x}")]
    ChecksumMismatch { expected: u32, actual: u32 },
}

pub type CodecResult<T> = std::result::Result<T, CodecError>;
