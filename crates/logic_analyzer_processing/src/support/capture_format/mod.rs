//! Common parsing and packed-sample helpers for capture-file support.

mod implementation;

pub(crate) use implementation::{get_packed_bit, parse_sample_rate};
