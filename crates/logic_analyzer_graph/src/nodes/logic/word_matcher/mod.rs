mod builder;
mod definition;
mod registration;

#[cfg(test)]
pub(crate) use definition::{WordMatcher, WordMatcherState};
#[cfg(test)]
pub(crate) use definition::{default_match_op, default_trigger_at};
