//! Random-access edge/value queries for a binary channel.
//!
//! [`EdgeQuery`] is the payload of the [`super::protocol::ProtocolKind::EdgeQuery`]
//! connection protocol: a channel-scoped, object-safe query surface a
//! consuming node can hold instead of a streamed [`super::sample::Sample`]
//! channel. One instance answers queries for exactly one channel, so a
//! future pass-through node (a logic gate, say) can hold one `Arc<dyn
//! EdgeQuery>` per input and build a new one for its own output purely by
//! combining them — `next_edge`/`value_at` are enough to do that lazily,
//! without ever streaming. Nothing here is wasm-gated: only the concrete
//! file-backed implementation in `nodes::dsl_file` is (it depends on the
//! native-only waveform index).
use crate::Result;
use crate::runtime::capture::CaptureTransition;

/// Random-access query surface for one binary/edge-valued channel.
pub trait EdgeQuery: Send + Sync {
    /// Sample period in seconds (for position <-> nanosecond conversion).
    fn sample_period(&self) -> f64;
    /// Sample rate in Hz. Kept alongside `sample_period` (rather than
    /// derived by callers as `1.0 / sample_period`) so a caller that needs
    /// to reproduce a streaming-path nanosecond timestamp bit-for-bit can
    /// use the exact same expression the streaming reader uses, without a
    /// second floating-point division introducing rounding drift.
    fn samplerate_hz(&self) -> f64;
    /// Total number of samples in the channel.
    fn total_samples(&self) -> u64;

    /// Value of the channel at `position`.
    fn value_at(&self, position: u64) -> Result<bool>;

    /// First transition strictly after `position`, at or before `limit`.
    /// `Ok(None)` if there isn't one before `limit`.
    fn next_edge(&self, position: u64, limit: u64) -> Result<Option<CaptureTransition>>;

    /// `next_edge` filtered to the first transition landing on `value`.
    /// Edges alternate, so the default implementation is at most two
    /// `next_edge` calls.
    fn next_edge_with_value(
        &self,
        position: u64,
        value: bool,
        limit: u64,
    ) -> Result<Option<CaptureTransition>> {
        let mut pos = position;
        loop {
            match self.next_edge(pos, limit)? {
                Some(transition) if transition.value == value => return Ok(Some(transition)),
                Some(transition) => pos = transition.sample,
                None => return Ok(None),
            }
        }
    }
}
