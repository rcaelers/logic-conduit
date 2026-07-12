use super::{CodecError, CodecResult};

pub const FORMAT_VERSION: u32 = 1;
pub const DATA_MAGIC: &[u8; 8] = b"DWRDDAT1";
pub const BLOCK_MAGIC: &[u8; 4] = b"DWBL";
pub const DATA_HEADER_SIZE: usize = 64;
pub const BLOCK_HEADER_SIZE: usize = 72;
pub const RESTART_ENTRY_SIZE: usize = 16;
pub const BLOCK_CHECKSUM_OFFSET: usize = 64;

pub const DEFAULT_MAX_WORDS_PER_BLOCK: usize = 65_536;
pub const DEFAULT_RESTART_INTERVAL: usize = 256;
pub const DEFAULT_MAX_BLOCK_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_INTER_WORD_GAP_NS: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataFileHeader {
    pub cache_key_prefix: [u8; 16],
    pub created_unix_ns: u64,
    pub flags: u64,
}

impl DataFileHeader {
    pub fn to_bytes(self) -> [u8; DATA_HEADER_SIZE] {
        let mut bytes = [0u8; DATA_HEADER_SIZE];
        bytes[..8].copy_from_slice(DATA_MAGIC);
        put_u32(&mut bytes, 8, FORMAT_VERSION);
        put_u32(&mut bytes, 12, DATA_HEADER_SIZE as u32);
        bytes[16..32].copy_from_slice(&self.cache_key_prefix);
        put_u64(&mut bytes, 32, self.created_unix_ns);
        put_u64(&mut bytes, 40, self.flags);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> CodecResult<Self> {
        if bytes.len() < DATA_HEADER_SIZE {
            return Err(CodecError::Truncated);
        }
        if &bytes[..8] != DATA_MAGIC {
            return Err(invalid("invalid data-file magic"));
        }
        if get_u32(bytes, 8)? != FORMAT_VERSION {
            return Err(invalid("unsupported data-file version"));
        }
        if get_u32(bytes, 12)? as usize != DATA_HEADER_SIZE {
            return Err(invalid("invalid data-file header size"));
        }
        if bytes[48..DATA_HEADER_SIZE].iter().any(|&byte| byte != 0) {
            return Err(invalid("non-zero reserved data-file header bytes"));
        }
        let mut cache_key_prefix = [0u8; 16];
        cache_key_prefix.copy_from_slice(&bytes[16..32]);
        Ok(Self {
            cache_key_prefix,
            created_unix_ns: get_u64(bytes, 32)?,
            flags: get_u64(bytes, 40)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordBlockHeader {
    pub flags: u16,
    pub sequence: u64,
    pub first_timestamp_ns: u64,
    pub last_timestamp_ns: u64,
    pub word_count: u32,
    pub value_bytes: u8,
    pub record_payload_len: u32,
    pub restart_count: u32,
    pub restart_table_offset: u32,
    pub duration_count: u32,
    pub duration_table_offset: u32,
    pub block_len: u32,
    pub crc32c: u32,
}

impl WordBlockHeader {
    pub(super) fn write_to(self, bytes: &mut [u8]) {
        debug_assert!(bytes.len() >= BLOCK_HEADER_SIZE);
        bytes[..BLOCK_HEADER_SIZE].fill(0);
        bytes[..4].copy_from_slice(BLOCK_MAGIC);
        put_u16(bytes, 4, BLOCK_HEADER_SIZE as u16);
        put_u16(bytes, 6, self.flags);
        put_u64(bytes, 8, self.sequence);
        put_u64(bytes, 16, self.first_timestamp_ns);
        put_u64(bytes, 24, self.last_timestamp_ns);
        put_u32(bytes, 32, self.word_count);
        bytes[36] = self.value_bytes;
        put_u32(bytes, 40, self.record_payload_len);
        put_u32(bytes, 44, self.restart_count);
        put_u32(bytes, 48, self.restart_table_offset);
        put_u32(bytes, 52, self.duration_count);
        put_u32(bytes, 56, self.duration_table_offset);
        put_u32(bytes, 60, self.block_len);
        put_u32(bytes, BLOCK_CHECKSUM_OFFSET, self.crc32c);
    }

    pub fn from_bytes(bytes: &[u8]) -> CodecResult<Self> {
        if bytes.len() < BLOCK_HEADER_SIZE {
            return Err(CodecError::Truncated);
        }
        if &bytes[..4] != BLOCK_MAGIC {
            return Err(invalid("invalid word-block magic"));
        }
        if get_u16(bytes, 4)? as usize != BLOCK_HEADER_SIZE {
            return Err(invalid("invalid word-block header size"));
        }
        if bytes[37..40].iter().any(|&byte| byte != 0)
            || bytes[68..72].iter().any(|&byte| byte != 0)
        {
            return Err(invalid("non-zero reserved word-block header bytes"));
        }
        Ok(Self {
            flags: get_u16(bytes, 6)?,
            sequence: get_u64(bytes, 8)?,
            first_timestamp_ns: get_u64(bytes, 16)?,
            last_timestamp_ns: get_u64(bytes, 24)?,
            word_count: get_u32(bytes, 32)?,
            value_bytes: bytes[36],
            record_payload_len: get_u32(bytes, 40)?,
            restart_count: get_u32(bytes, 44)?,
            restart_table_offset: get_u32(bytes, 48)?,
            duration_count: get_u32(bytes, 52)?,
            duration_table_offset: get_u32(bytes, 56)?,
            block_len: get_u32(bytes, 60)?,
            crc32c: get_u32(bytes, BLOCK_CHECKSUM_OFFSET)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartEntry {
    pub timestamp_ns: u64,
    /// Byte offset relative to the start of the record payload.
    pub payload_offset: u32,
    pub record_index: u32,
}

/// One fully written block published by the live store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDirectoryEntry {
    pub sequence: u64,
    pub first_timestamp_ns: u64,
    pub last_timestamp_ns: u64,
    pub data_offset: u64,
    pub block_len: u32,
    pub word_count: u32,
    pub value_bytes: u8,
    pub flags: u8,
}

impl RestartEntry {
    pub(super) fn append_to(self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.timestamp_ns.to_le_bytes());
        output.extend_from_slice(&self.payload_offset.to_le_bytes());
        output.extend_from_slice(&self.record_index.to_le_bytes());
    }

    pub(super) fn read_from(bytes: &[u8], offset: usize) -> CodecResult<Self> {
        Ok(Self {
            timestamp_ns: get_u64(bytes, offset)?,
            payload_offset: get_u32(bytes, offset + 8)?,
            record_index: get_u32(bytes, offset + 12)?,
        })
    }
}

fn invalid(message: &str) -> CodecError {
    CodecError::InvalidFormat(message.to_string())
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(bytes: &[u8], offset: usize) -> CodecResult<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or(CodecError::Truncated)?
        .try_into()
        .expect("fixed-size slice");
    Ok(u16::from_le_bytes(value))
}

fn get_u32(bytes: &[u8], offset: usize) -> CodecResult<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or(CodecError::Truncated)?
        .try_into()
        .expect("fixed-size slice");
    Ok(u32::from_le_bytes(value))
}

fn get_u64(bytes: &[u8], offset: usize) -> CodecResult<u64> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or(CodecError::Truncated)?
        .try_into()
        .expect("fixed-size slice");
    Ok(u64::from_le_bytes(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_file_header_round_trips_exactly() {
        let header = DataFileHeader {
            cache_key_prefix: *b"0123456789abcdef",
            created_unix_ns: 123_456_789,
            flags: 7,
        };
        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), DATA_HEADER_SIZE);
        assert_eq!(DataFileHeader::from_bytes(&bytes).unwrap(), header);
    }
}
