//! Sigrok capture-file source node.

mod implementation;

pub use implementation::{
    SigrokCaptureReader, SigrokChunkedCaptureReader, SigrokFileCaptureDataSource, SigrokFileSource,
    open_sigrok_chunked_capture,
};
