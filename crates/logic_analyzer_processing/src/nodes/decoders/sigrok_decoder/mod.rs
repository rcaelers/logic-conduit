//! Sigrok Python decoder runtime and collected output contracts.

#[cfg(not(target_arch = "wasm32"))]
mod implementation;
mod output_payloads;

#[cfg(not(target_arch = "wasm32"))]
pub use implementation::{
    SigrokChannel, SigrokDecoder, SigrokDecoderConfig, SigrokInitialPin, SigrokOptionValue,
};
pub use output_payloads::{
    SigrokAnnotation, SigrokBinary, SigrokGeneratedLogic, SigrokLaneSnapshot, SigrokMetadata,
    SigrokMetadataValue, SigrokProtocolPacket, SigrokValue, sigrok_annotation_payload_adapter,
    sigrok_binary_payload_adapter, sigrok_generated_logic_payload_adapter,
    sigrok_metadata_payload_adapter, sigrok_protocol_packet_payload_adapter,
};
