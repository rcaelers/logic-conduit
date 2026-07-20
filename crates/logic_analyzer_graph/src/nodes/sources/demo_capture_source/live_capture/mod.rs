mod implementation;

#[path = "native.rs"]
mod platform;

pub(crate) use implementation::feature;
