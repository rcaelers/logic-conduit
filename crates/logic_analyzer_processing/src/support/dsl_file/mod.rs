//! Random-access support for DSLogic `.dsl` capture files.

mod implementation;

#[cfg(test)]
pub(crate) use implementation::DslCaptureReader;
#[cfg(test)]
pub(crate) use implementation::parse_sample_rate;
pub(crate) use implementation::{
    DslChunkedCaptureReader, DslFileCaptureDataSource, get_bit, parse_header, 
};
