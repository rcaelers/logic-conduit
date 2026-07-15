//! Event and level-stream types for control-path channels.
//!
//! Two kinds of stream flow between control nodes (see
//! `docs/PIPELINE_DESIGN.md`):
//!
//! - **Events** ([`Trigger`], [`Word`]): timestamped occurrences with no
//!   value between occurrences. They can only be reacted to.
//! - **Stepped levels** ([`NumberSample`], [`TextSample`]): a value defined at
//!   *every* instant, transmitted as changes only — the same RLE idea as
//!   [`Sample`](super::sample::Sample). Every level producer emits its initial
//!   value at t=0 on its first `work()` call, and consumers hold the last
//!   received value, so a consumer never has to wait for a level to *exist*.
//!
//! All timestamps are nanoseconds, in the same domain as `Sample.start_time_ns`.

/// Longest inferred display span for an instantaneous word when no recent
/// cadence is available. Prevents sparse word events from painting a value
/// continuously across an unrelated or gated-off interval.
pub const MAX_ANNOTATION_NS: u64 = 100_000_000;

/// Returns the visual end of an instantaneous word with a known successor.
///
/// Adjacent words in a burst still meet exactly. When the next word is much
/// later than the recent cadence, the current word closes after one expected
/// period so the intervening interval remains visibly empty.
pub fn instantaneous_word_end_ns(
    previous_start_ns: Option<u64>,
    start_ns: u64,
    next_start_ns: u64,
) -> u64 {
    let gap_ns = next_start_ns.saturating_sub(start_ns);
    let inferred_limit_ns = previous_start_ns
        .map(|previous| start_ns.saturating_sub(previous))
        .filter(|interval| *interval > 0)
        .unwrap_or(MAX_ANNOTATION_NS)
        .min(MAX_ANNOTATION_NS);
    start_ns.saturating_add(gap_ns.min(inferred_limit_ns))
}

/// Instantaneous event (e.g. a matcher hit). No payload beyond time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trigger {
    /// Timestamp in nanoseconds.
    pub timestamp_ns: u64,
}

impl Trigger {
    pub fn new(timestamp_ns: u64) -> Self {
        Self { timestamp_ns }
    }
}

/// A single decoded word from any serial/parallel decoder (SPI, parallel
/// bus, UART, I2C, …) — every decoder's output reduces to this: one value
/// up to 64 bits wide, timestamped where it started. No decoder needs a
/// payload type of its own beyond this — a decoder that wants to expose
/// two independent word-shaped things (e.g. SPI's MOSI and MISO) does so
/// via two output ports, not two fields on one struct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Word {
    pub value: u64,
    /// Timestamp of the word's start (its first sampling edge), ns.
    pub timestamp_ns: u64,
    /// The word's real extent: start to its last sampling edge / frame
    /// end, ns. `0` means instantaneous. The viewer joins adjacent
    /// instantaneous words within a decode burst, but leaves long gaps
    /// empty rather than implying valid decoded data while a gate is off.
    pub duration_ns: u64,
}

impl Word {
    /// An instantaneous word (`duration_ns == 0`).
    pub fn new(value: u64, timestamp_ns: u64) -> Self {
        Self {
            value,
            timestamp_ns,
            duration_ns: 0,
        }
    }

    /// A word spanning `[timestamp_ns, timestamp_ns + duration_ns]`.
    pub fn spanning(value: u64, timestamp_ns: u64, duration_ns: u64) -> Self {
        Self {
            value,
            timestamp_ns,
            duration_ns,
        }
    }

    /// The word's end (equals its start for instantaneous words).
    pub fn end_ns(&self) -> u64 {
        self.timestamp_ns + self.duration_ns
    }
}

/// A decoded word prepared for timeline rendering. Instantaneous words use
/// the next word's timestamp as `end_ns`; explicitly-spanning words retain
/// their encoded duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub start_ns: u64,
    pub end_ns: u64,
    pub value: u64,
}

/// Change of an integer level (e.g. counter output). Mirrors `Sample`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NumberSample {
    /// The level's value from `start_time_ns` until the next change.
    pub value: i64,
    /// Timestamp in nanoseconds when this value started.
    pub start_time_ns: u64,
}

impl NumberSample {
    pub fn new(value: i64, start_time_ns: u64) -> Self {
        Self {
            value,
            start_time_ns,
        }
    }
}

/// Change of a text level (e.g. formatter output / filename). Mirrors `Sample`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSample {
    /// The level's value from `start_time_ns` until the next change.
    pub value: String,
    /// Timestamp in nanoseconds when this value started.
    pub start_time_ns: u64,
}

impl TextSample {
    pub fn new(value: impl Into<String>, start_time_ns: u64) -> Self {
        Self {
            value: value.into(),
            start_time_ns,
        }
    }
}
