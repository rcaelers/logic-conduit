//! Control-path logic processing nodes.
//!
//! Small composable nodes that turn decoded word streams into control levels
//! (see `docs/PIPELINE_DESIGN.md`):
//!
//! - [`WordMatcher`] — word stream → [`Trigger`](signal_processing::Trigger)
//!   events on pattern match
//! - [`SrLatch`] — set/reset triggers → boolean level
//! - [`LogicGate`] — N boolean levels → boolean level (AND/OR/… per
//!   [`GateOp`])
//! - [`TriggerCounter`] — triggers → integer level
//! - [`TextFormatter`] — integer level → text level (template substitution)
//!
//! All level outputs follow the level-stream contract: the initial value is
//! emitted at t=0 on the first `work()` call, then changes only.
//!
//! Multi-input merge semantics differ by stream kind:
//!
//! - **Trigger inputs** ([`SrLatch`]) merge in strict timestamp order. Each
//!   input must provide its next event or close before the latch advances;
//!   this prevents independent matcher threads from applying a later Set
//!   before an earlier Reset. Consequently, a live sparse branch can delay
//!   the other branch until its next event; finite decode pipelines close
//!   both streams and remain exact.
//! - **Level inputs** ([`LogicGate`]) merge in *strict timestamp order*,
//!   blocking on the input whose next edge is unknown. Levels make this safe
//!   (each input either advances or closes) and it is required for
//!   correctness: input arrival skew is unbounded — a raw source channel
//!   runs megabytes ahead of a decode-derived control level — and an
//!   event-driven merge would consume the fast input far past the slow one,
//!   mass-clamping its edges and corrupting the output timeline.

mod buffer;
mod formatter;
mod logic_gate;
mod sr_latch;
mod trigger_counter;
mod word_matcher;

pub use buffer::BufferNode;
pub use formatter::TextFormatter;
pub use logic_gate::{GateOp, LogicGate};
pub use sr_latch::SrLatch;
pub use trigger_counter::TriggerCounter;
pub use word_matcher::{MatchOp, TriggerAt, WordMatcher};
