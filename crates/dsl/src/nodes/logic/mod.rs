//! Control-path logic nodes
//!
//! Small composable nodes that turn decoded word streams into control levels
//! (see `ANALYSIS_PIPELINE_DESIGN.md` §4.2–§4.7):
//!
//! - [`WordMatcher`] — word stream → [`Trigger`](crate::runtime::Trigger)
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
//! - **Trigger inputs** ([`SrLatch`]) merge *event-driven*: process whichever
//!   input has data, ordering only the items available at that moment. A
//!   strict merge would starve — a trigger stream carries no "nothing
//!   happened" information, so a set edge could not be emitted until the
//!   *next* reset arrived. Out-of-order arrivals are clamped + logged
//!   (set/reset derive from the same word stream, so their skew is
//!   protocol-scale).
//! - **Level inputs** ([`LogicGate`]) merge in *strict timestamp order*,
//!   blocking on the input whose next edge is unknown. Levels make this safe
//!   (each input either advances or closes) and it is required for
//!   correctness: input arrival skew is unbounded — a raw source channel
//!   runs megabytes ahead of a decode-derived control level — and an
//!   event-driven merge would consume the fast input far past the slow one,
//!   mass-clamping its edges and corrupting the output timeline.

mod formatter;
mod logic_gate;
mod sr_latch;
mod trigger_counter;
mod word_matcher;

pub use formatter::TextFormatter;
pub use logic_gate::{GateOp, LogicGate};
pub use sr_latch::SrLatch;
pub use trigger_counter::TriggerCounter;
pub use word_matcher::{WordField, WordMatcher, WordSource};
