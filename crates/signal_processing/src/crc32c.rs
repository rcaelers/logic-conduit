//! Shared CRC-32C integrity checks for persistent binary formats.

const POLYNOMIAL: u32 = 0x82f6_3b78;

const fn tables() -> [[u32; 256]; 8] {
    let mut result = [[0u32; 256]; 8];
    let mut index = 0;
    while index < result[0].len() {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLYNOMIAL
            } else {
                crc >> 1
            };
            bit += 1;
        }
        result[0][index] = crc;
        index += 1;
    }
    let mut table = 1;
    while table < result.len() {
        let mut index = 0;
        while index < result[table].len() {
            let previous = result[table - 1][index];
            result[table][index] =
                result[0][(previous & 0xff) as usize] ^ (previous >> 8);
            index += 1;
        }
        table += 1;
    }
    result
}

const TABLES: [[u32; 256]; 8] = tables();

#[inline]
fn update(mut crc: u32, bytes: &[u8]) -> u32 {
    let (chunks, remainder) = bytes.as_chunks::<8>();
    for chunk in chunks {
        crc ^= u32::from_le_bytes(chunk[..4].try_into().expect("four-byte CRC prefix"));
        crc = TABLES[7][(crc & 0xff) as usize]
            ^ TABLES[6][((crc >> 8) & 0xff) as usize]
            ^ TABLES[5][((crc >> 16) & 0xff) as usize]
            ^ TABLES[4][(crc >> 24) as usize]
            ^ TABLES[3][chunk[4] as usize]
            ^ TABLES[2][chunk[5] as usize]
            ^ TABLES[1][chunk[6] as usize]
            ^ TABLES[0][chunk[7] as usize];
    }
    for &byte in remainder {
        crc = TABLES[0][((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    crc
}

pub(crate) fn checksum_parts(parts: &[&[u8]]) -> u32 {
    let mut crc = !0;
    for part in parts {
        crc = update(crc, part);
    }
    !crc
}

/// Computes a block checksum while treating its stored checksum field as zero.
pub(crate) fn block_checksum(bytes: &[u8], checksum_offset: usize) -> u32 {
    let mut crc = update(!0, &bytes[..checksum_offset]);
    crc = update(crc, &[0; size_of::<u32>()]);
    crc = update(crc, &bytes[checksum_offset + size_of::<u32>()..]);
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checksum(bytes: &[u8]) -> u32 {
        checksum_parts(&[bytes])
    }

    fn bytewise_checksum(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc = TABLES[0][((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
        }
        !crc
    }

    #[test]
    fn crc32c_matches_the_standard_check_value() {
        assert_eq!(checksum(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn checksum_parts_matches_the_concatenated_input() {
        assert_eq!(
            checksum_parts(&[b"123", b"456", b"789"]),
            checksum(b"123456789")
        );
    }

    #[test]
    fn slicing_by_eight_matches_bytewise_for_all_tail_lengths() {
        let bytes: Vec<_> = (0..=255).map(|value| value as u8).collect();
        for length in 0..=bytes.len() {
            assert_eq!(checksum(&bytes[..length]), bytewise_checksum(&bytes[..length]));
        }
    }
}
