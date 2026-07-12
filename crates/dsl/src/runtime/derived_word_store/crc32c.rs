const POLYNOMIAL: u32 = 0x82f6_3b78;

const fn table() -> [u32; 256] {
    let mut result = [0u32; 256];
    let mut index = 0;
    while index < result.len() {
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
        result[index] = crc;
        index += 1;
    }
    result
}

const TABLE: [u32; 256] = table();

#[inline]
fn update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc = TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    crc
}

#[cfg(test)]
pub(super) fn checksum(bytes: &[u8]) -> u32 {
    !update(!0, bytes)
}

/// Computes a block checksum while treating its stored checksum field as zero.
pub(super) fn block_checksum(bytes: &[u8], checksum_offset: usize) -> u32 {
    let mut crc = update(!0, &bytes[..checksum_offset]);
    crc = update(crc, &[0; size_of::<u32>()]);
    crc = update(crc, &bytes[checksum_offset + size_of::<u32>()..]);
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_matches_the_standard_check_value() {
        assert_eq!(checksum(b"123456789"), 0xe306_9283);
    }
}
