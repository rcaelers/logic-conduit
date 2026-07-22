//! Random-access sigrok v2 (`.sr`) capture-file support.

mod implementation;

#[cfg(test)]
pub(crate) use implementation::SigrokCaptureReader;
pub(crate) use implementation::{SigrokCapture, SigrokFileCaptureDataSource};
