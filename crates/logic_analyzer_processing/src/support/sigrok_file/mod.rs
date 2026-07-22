//! Random-access sigrok v2 (`.sr`) capture-file support.

mod implementation;

pub(crate) use implementation::{SigrokCapture, SigrokFileCaptureDataSource};
