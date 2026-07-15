mod builder;
mod definition;

pub(crate) use builder::WordMatcherBuilder;
pub use definition::{WordMatcher, WordMatcherState, default_match_op, default_trigger_at};
