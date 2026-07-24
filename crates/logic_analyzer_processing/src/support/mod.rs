pub(crate) mod capture_format;
pub(crate) mod capture_index;
pub(crate) mod dsl_file;
pub mod logic_analyzer;
pub(crate) mod sigrok_file;
mod sigrokdecode;

pub(crate) use capture_format::{get_packed_bit, parse_sample_rate};
pub(crate) use sigrokdecode::{
    DecoderOutput, DecoderWorker, InitialPin, LogicChunk, MetadataType, OUTPUT_ANN, OUTPUT_BINARY,
    OUTPUT_LOGIC, OUTPUT_META, OUTPUT_PYTHON, OptionValue, OutputRegistration, WorkerConfig,
    WorkerError,
};
pub use sigrokdecode::{
    SigrokAnnotationClassDescriptor, SigrokAnnotationRowDescriptor, SigrokCatalogDiagnostic,
    SigrokCatalogDiagnosticKind, SigrokCatalogEntry, SigrokCatalogSnapshot, SigrokDecoderCatalog,
    SigrokDecoderChannelDescriptor, SigrokDecoderDescriptor, SigrokDecoderOptionDescriptor,
    SigrokOutputKind, SigrokScalarValue, discover_sigrok_decoder,
};
