use super::errors::{CodecError, CodecResult};

/// Encodes an unsigned integer using canonical unsigned LEB128.
pub fn encode_u64(mut value: u64, output: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            return;
        }
    }
}

/// Decodes one unsigned LEB128 integer and advances `cursor`.
pub fn decode_u64(input: &[u8], cursor: &mut usize) -> CodecResult<u64> {
    let mut value = 0u64;
    for byte_index in 0..10 {
        let byte = *input.get(*cursor).ok_or(CodecError::Truncated)?;
        *cursor += 1;
        if byte_index == 9 && byte > 1 {
            return Err(CodecError::VlqOverflow);
        }
        value |= u64::from(byte & 0x7f) << (byte_index * 7);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(CodecError::VlqOverflow)
}

pub const fn encoded_len(value: u64) -> usize {
    let significant_bits = if value == 0 {
        1
    } else {
        (u64::BITS - value.leading_zeros()) as usize
    };
    significant_bits.div_ceil(7)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_vlq_round_trips_boundaries() {
        for value in [0, 1, 0x7f, 0x80, 0x3fff, 0x4000, u32::MAX as u64, u64::MAX] {
            let mut encoded = Vec::new();
            encode_u64(value, &mut encoded);
            assert_eq!(encoded.len(), encoded_len(value));
            let mut cursor = 0;
            assert_eq!(decode_u64(&encoded, &mut cursor).unwrap(), value);
            assert_eq!(cursor, encoded.len());
        }
    }

    #[test]
    fn unsigned_vlq_rejects_truncation_and_overflow() {
        let mut cursor = 0;
        assert_eq!(decode_u64(&[0x80], &mut cursor), Err(CodecError::Truncated));

        let mut cursor = 0;
        assert_eq!(
            decode_u64(&[0xff; 10], &mut cursor),
            Err(CodecError::VlqOverflow)
        );
    }
}
