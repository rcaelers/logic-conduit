//! Native host infrastructure for Sigrok Python decoders without libsigrokdecode.

#[allow(dead_code)]
mod bridge;
#[allow(dead_code)]
mod conditions;
mod discovery;
#[allow(dead_code)]
mod python_host;
#[allow(dead_code)]
mod scheduler;
#[allow(dead_code)]
mod worker;

pub(crate) use bridge::{DecoderOutput, MetadataType, OutputRegistration};
pub use discovery::{
    SigrokAnnotationClassDescriptor, SigrokAnnotationRowDescriptor, SigrokDecoderChannelDescriptor,
    SigrokDecoderDescriptor, SigrokDecoderOptionDescriptor, SigrokOutputKind, SigrokScalarValue,
    discover_sigrok_decoder,
};
pub(crate) use python_host::{OUTPUT_ANN, OUTPUT_BINARY, OUTPUT_LOGIC, OUTPUT_META, OUTPUT_PYTHON};
pub(crate) use scheduler::{InitialPin, LogicChunk};
pub(crate) use worker::{DecoderWorker, OptionValue, WorkerConfig, WorkerError};

#[cfg(test)]
mod worker_tests;
