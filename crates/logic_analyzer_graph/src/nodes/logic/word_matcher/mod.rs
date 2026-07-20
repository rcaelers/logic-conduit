mod builder;
mod definition;

pub(crate) use builder::WordMatcherBuilder;
pub use definition::{WordMatcher, WordMatcherState};
#[cfg(test)]
pub(crate) use definition::{default_match_op, default_trigger_at};
