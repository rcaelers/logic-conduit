//! Test-only live-capture platform facade.

mod implementation;

#[path = "native.rs"]
mod platform;

pub(crate) use implementation::feature;
