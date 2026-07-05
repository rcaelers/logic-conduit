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
//! Multi-input nodes are *event-driven*, not strict timestamp merges: they
//! process whichever input has data (holding every input's current level)
//! and order only the items that are available at that moment. A strict
//! merge would starve on sparse inputs — it could not emit a set edge until
//! the *next* reset arrived. Out-of-order arrivals across inputs are clamped
//! to the last emitted timestamp and logged.

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
