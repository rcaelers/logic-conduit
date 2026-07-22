//! Random-access support for DSLogic `.dsl` capture files.

mod implementation;

#[cfg(test)]
pub(crate) use implementation::DslCaptureReader;
pub(crate) use implementation::{DslChunkedCaptureReader, DslFileCaptureDataSource, parse_header};
