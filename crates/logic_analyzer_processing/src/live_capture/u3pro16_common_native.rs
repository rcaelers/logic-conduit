//! Shared lossless stream adaptation for native U3Pro16 acquisition profiles.

use std::sync::Arc;

use signal_processing::CaptureBytes;

use crate::nodes::{LogicAnalyzerError, LogicChunk, LogicEncoding};

use super::{AcquisitionError, AcquisitionResult};

pub(super) struct CanonicalTransfer {
    pub bytes: CaptureBytes,
    pub sample_count: u64,
}

impl CanonicalTransfer {
    pub(super) fn limit_samples(
        self,
        maximum: u64,
        channel_count: usize,
    ) -> AcquisitionResult<Self> {
        if self.sample_count <= maximum {
            return Ok(self);
        }
        let bit_count = usize::try_from(
            u128::from(maximum)
                .checked_mul(channel_count as u128)
                .ok_or_else(|| AcquisitionError::Protocol("sample limit overflow".into()))?,
        )
        .map_err(|_| AcquisitionError::Protocol("sample limit is too large".into()))?;
        let mut bytes = self.bytes.as_slice()[..bit_count.div_ceil(8)].to_vec();
        if !bit_count.is_multiple_of(8) {
            *bytes.last_mut().unwrap() &= (1 << (bit_count % 8)) - 1;
        }
        Ok(Self {
            bytes: bytes.into(),
            sample_count: maximum,
        })
    }
}

#[derive(Default)]
pub(super) struct CanonicalTransferAssembler {
    input_bits: u64,
    carry: Vec<bool>,
}

impl CanonicalTransferAssembler {
    pub(super) fn push(
        &mut self,
        chunk: &LogicChunk,
        channel_count: usize,
    ) -> AcquisitionResult<Option<CanonicalTransfer>> {
        if channel_count == 0 || usize::from(chunk.channel_count) != channel_count {
            return Err(AcquisitionError::Protocol(
                "U3Pro16 upload channel count changed unexpectedly".into(),
            ));
        }
        if chunk.encoding != LogicEncoding::InterleavedLsbFirst {
            return Err(AcquisitionError::Protocol(
                "U3Pro16 upload encoding changed unexpectedly".into(),
            ));
        }
        if chunk.start_bit != self.input_bits {
            return Err(AcquisitionError::Integrity(format!(
                "U3Pro16 upload starts at bit {}, expected {}",
                chunk.start_bit, self.input_bits
            )));
        }
        self.input_bits = self
            .input_bits
            .checked_add(chunk.bit_len as u64)
            .ok_or_else(|| AcquisitionError::Protocol("upload bit count overflow".into()))?;
        if chunk.bit_len == 0 {
            return Ok(None);
        }

        let transfer = canonicalize_transfer(&self.carry, chunk, channel_count)?;
        self.carry = transfer.1;
        Ok((transfer.0.sample_count != 0).then_some(transfer.0))
    }

    pub(super) fn finish(&self) -> AcquisitionResult<()> {
        if self.carry.is_empty() {
            Ok(())
        } else {
            Err(AcquisitionError::Integrity(format!(
                "U3Pro16 upload ended with {} bits of an incomplete sample",
                self.carry.len()
            )))
        }
    }
}

fn canonicalize_transfer(
    carry: &[bool],
    chunk: &LogicChunk,
    channel_count: usize,
) -> AcquisitionResult<(CanonicalTransfer, Vec<bool>)> {
    let bit_offset = usize::from(chunk.bit_offset);
    let available_bits = chunk
        .data
        .len()
        .checked_mul(8)
        .and_then(|bits| bits.checked_sub(bit_offset))
        .ok_or_else(|| AcquisitionError::Protocol("invalid upload bit span".into()))?;
    if chunk.bit_len > available_bits {
        return Err(AcquisitionError::Protocol(
            "U3Pro16 upload bit span exceeds its transfer buffer".into(),
        ));
    }
    let total_bits = carry
        .len()
        .checked_add(chunk.bit_len)
        .ok_or_else(|| AcquisitionError::Protocol("upload bit count overflow".into()))?;
    let sample_count = total_bits / channel_count;
    let complete_bits = sample_count * channel_count;
    if complete_bits == 0 {
        let mut next_carry = carry.to_vec();
        next_carry.extend((0..chunk.bit_len).map(|bit| chunk.bit(bit)));
        return Ok((
            CanonicalTransfer {
                bytes: Vec::new().into(),
                sample_count: 0,
            },
            next_carry,
        ));
    }

    if carry.is_empty()
        && bit_offset == 0
        && complete_bits == chunk.bit_len
        && chunk.bit_len == chunk.data.len() * 8
    {
        return Ok((
            CanonicalTransfer {
                bytes: CaptureBytes::from(Arc::clone(&chunk.data)),
                sample_count: sample_count as u64,
            },
            Vec::new(),
        ));
    }

    let mut bytes = vec![0_u8; complete_bits.div_ceil(8)];
    for (bit, level) in carry.iter().copied().enumerate() {
        if level {
            bytes[bit / 8] |= 1 << (bit % 8);
        }
    }

    let source_bits = complete_bits - carry.len();
    let destination_shift = carry.len() % 8;
    for source_byte in 0..source_bits.div_ceil(8) {
        let source_bit = source_byte * 8;
        let absolute_bit = bit_offset + source_bit;
        let data_byte = absolute_bit / 8;
        let source_shift = absolute_bit % 8;
        let mut value = chunk.data[data_byte] >> source_shift;
        if source_shift != 0
            && let Some(next) = chunk.data.get(data_byte + 1)
        {
            value |= *next << (8 - source_shift);
        }
        let destination_bit = carry.len() + source_bit;
        let destination_byte = destination_bit / 8;
        bytes[destination_byte] |= value << destination_shift;
        if destination_shift != 0
            && let Some(next) = bytes.get_mut(destination_byte + 1)
        {
            *next |= value >> (8 - destination_shift);
        }
    }

    if !complete_bits.is_multiple_of(8) {
        *bytes.last_mut().unwrap() &= (1 << (complete_bits % 8)) - 1;
    }
    let next_carry = (source_bits..chunk.bit_len)
        .map(|bit| chunk.bit(bit))
        .collect();
    Ok((
        CanonicalTransfer {
            bytes: bytes.into(),
            sample_count: sample_count as u64,
        },
        next_carry,
    ))
}

pub(super) fn map_analyzer_error(error: LogicAnalyzerError) -> AcquisitionError {
    match error {
        LogicAnalyzerError::InvalidSettings(message) => AcquisitionError::InvalidRequest(message),
        LogicAnalyzerError::Transport(message) | LogicAnalyzerError::Timeout(message) => {
            AcquisitionError::Transport(message)
        }
        LogicAnalyzerError::Protocol(message) => AcquisitionError::Protocol(message),
        LogicAnalyzerError::Integrity(message) => AcquisitionError::Integrity(message),
        LogicAnalyzerError::NotCapturing => {
            AcquisitionError::Protocol("U3Pro16 capture is not active".into())
        }
    }
}
