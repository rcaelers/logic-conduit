//! Event and level stream types for control-path channels
//!
//! Two kinds of stream flow between control nodes (see
//! `ANALYSIS_PIPELINE_DESIGN.md` §3.1):
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
    /// end, ns. `0` means instantaneous — a single-cycle parallel bus
    /// word, whose value stands on the bus until the next strobe; the
    /// viewer renders those as lasting until the next word, while a word
    /// with a duration is drawn exactly that wide.
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
        Self { value, start_time_ns }
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
