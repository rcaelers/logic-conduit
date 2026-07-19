//! Random-access edge and value queries for a binary channel.
//!
//! [`EdgeQuery`] is the payload of the [`super::protocol::ProtocolKind::EdgeQuery`]
//! connection protocol: a channel-scoped, object-safe query surface a
//! consuming node can hold instead of a streamed [`super::sample::Sample`]
//! channel. One instance answers queries for exactly one channel, so a
//! pass-through or combining node can hold one `Arc<dyn EdgeQuery>` per input
//! and build a new one for its own output purely by
//! combining them — `next_edge`/`value_at` are enough to do that lazily,
//! without ever streaming. The contract is platform-neutral; implementations
//! that require a native index are selected at their implementation boundary.
use crate::Result;
use crate::capture::CaptureTransition;

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

    /// Fraction of 64-sample index groups containing at least one
    /// transition, when the concrete source can answer from summary data.
    /// `None` means no inexpensive density hint is available.
    fn activity_ratio_hint(&self) -> Option<f64> {
        None
    }

    /// Value of the channel at `position`.
    fn value_at(&self, position: u64) -> Result<bool>;

    /// First transition strictly after `position`, at or before `limit`.
    /// `Ok(None)` if there isn't one before `limit`.
    fn next_edge(&self, position: u64, limit: u64) -> Result<Option<CaptureTransition>>;

    /// Appends up to `max_edges` transitions strictly after `position` and
    /// before `limit`. The output is cleared first and transitions are
    /// ordered by sample position.
    ///
    /// Computed query sources get a correct scalar fallback. File-backed
    /// sources override this to hold their index/cache state once for the
    /// complete batch.
    fn next_edges(
        &self,
        position: u64,
        limit: u64,
        max_edges: usize,
        output: &mut Vec<CaptureTransition>,
    ) -> Result<()> {
        output.clear();
        if max_edges == 0 {
            return Ok(());
        }

        let mut cursor = position;
        while output.len() < max_edges {
            let Some(transition) = self.next_edge(cursor, limit)? else {
                break;
            };
            cursor = transition.sample;
            output.push(transition);
        }
        Ok(())
    }

    /// Reads this channel at every position in `positions`, preserving input
    /// order. The output is cleared first.
    ///
    /// The default implementation performs scalar point reads. File-backed
    /// sources override it to group sorted positions by packed block.
    fn values_at(&self, positions: &[u64], output: &mut Vec<bool>) -> Result<()> {
        output.clear();
        output.reserve(positions.len());
        for &position in positions {
            output.push(self.value_at(position)?);
        }
        Ok(())
    }

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
