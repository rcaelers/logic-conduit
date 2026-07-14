use super::config::BlockCodecConfig;
use super::crc32c::block_checksum;
use super::format::{
    BLOCK_CHECKSUM_OFFSET, BLOCK_FLAG_HAS_DURATIONS, BLOCK_HEADER_SIZE,
    DEFAULT_MAX_WORDS_PER_BLOCK, RESTART_ENTRY_SIZE, RestartEntry, WordBlockHeader,
};
use super::vlq::{decode_u64, encode_u64, encoded_len};
use super::errors::{CodecError, CodecResult};
use crate::runtime::events::Word;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushResult {
    Appended,
    BlockFull,
}

/// Accumulates one ordered block and predicts configured block boundaries.
#[derive(Debug)]
pub struct WordBlockBuilder {
    config: BlockCodecConfig,
    words: Vec<Word>,
    timestamp_bytes: usize,
    duration_bytes: usize,
    duration_count: usize,
    last_duration_index: usize,
    max_value: u64,
}

impl WordBlockBuilder {
    pub fn new(config: BlockCodecConfig) -> CodecResult<Self> {
        if config.restart_interval == 0 {
            return Err(CodecError::InvalidRestartInterval);
        }
        if config.max_words == 0 {
            return Err(CodecError::InvalidConfiguration(
                "max_words must be greater than zero",
            ));
        }
        if config.max_words > u32::MAX as usize {
            return Err(CodecError::InvalidConfiguration(
                "max_words must fit in u32",
            ));
        }
        if config.max_payload_bytes == 0 {
            return Err(CodecError::InvalidConfiguration(
                "max_payload_bytes must be greater than zero",
            ));
        }
        Ok(Self {
            config,
            words: Vec::with_capacity(config.max_words.min(DEFAULT_MAX_WORDS_PER_BLOCK)),
            timestamp_bytes: 0,
            duration_bytes: 0,
            duration_count: 0,
            last_duration_index: 0,
            max_value: 0,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn words(&self) -> &[Word] {
        &self.words
    }

    /// Appends `word`, or reports that the current non-empty block should be
    /// committed first. `word` is not consumed when `BlockFull` is returned.
    pub fn push(&mut self, word: Word) -> CodecResult<PushResult> {
        self.validate_order(word)?;
        if !self.words.is_empty() && self.would_close_before(word) {
            return Ok(PushResult::BlockFull);
        }
        if self.words.len() == u32::MAX as usize {
            return Err(CodecError::TooManyWords(self.words.len() + 1));
        }
        self.append(word);
        Ok(PushResult::Appended)
    }

    pub fn clear(&mut self) {
        self.words.clear();
        self.timestamp_bytes = 0;
        self.duration_bytes = 0;
        self.duration_count = 0;
        self.last_duration_index = 0;
        self.max_value = 0;
    }

    pub fn encode(&self, sequence: u64, output: &mut Vec<u8>) -> CodecResult<EncodedBlockMetadata> {
        encode_word_block_with_interval(sequence, &self.words, self.config.restart_interval, output)
    }

    fn validate_order(&self, word: Word) -> CodecResult<()> {
        if let Some(previous) = self.words.last()
            && word.timestamp_ns < previous.timestamp_ns
        {
            return Err(CodecError::OutOfOrder {
                index: self.words.len(),
                previous_timestamp_ns: previous.timestamp_ns,
                timestamp_ns: word.timestamp_ns,
            });
        }
        Ok(())
    }

    fn would_close_before(&self, word: Word) -> bool {
        let first = self.words.first().expect("non-empty builder");
        let last = self.words.last().expect("non-empty builder");
        if self.words.len() >= self.config.max_words
            || word.timestamp_ns - last.timestamp_ns > self.config.max_inter_word_gap_ns
            || word.timestamp_ns - first.timestamp_ns > self.config.max_timestamp_span_ns
        {
            return true;
        }

        let next_index = self.words.len();
        let timestamp_bytes =
            self.timestamp_bytes + encoded_len(word.timestamp_ns.saturating_sub(last.timestamp_ns));
        let value_bytes = value_width(self.max_value.max(word.value));
        let duration_bytes = self.duration_bytes
            + if word.duration_ns == 0 {
                0
            } else {
                let index_delta = if self.duration_count == 0 {
                    next_index
                } else {
                    next_index - self.last_duration_index
                };
                encoded_len(index_delta as u64) + encoded_len(word.duration_ns)
            };
        let record_bytes = timestamp_bytes + (next_index + 1) * value_bytes;
        let restart_count = (next_index + 1).div_ceil(self.config.restart_interval);
        record_bytes + restart_count * RESTART_ENTRY_SIZE + duration_bytes
            > self.config.max_payload_bytes
    }

    fn append(&mut self, word: Word) {
        let index = self.words.len();
        let delta = self
            .words
            .last()
            .map_or(0, |previous| word.timestamp_ns - previous.timestamp_ns);
        self.timestamp_bytes += encoded_len(delta);
        if word.duration_ns != 0 {
            let index_delta = if self.duration_count == 0 {
                index
            } else {
                index - self.last_duration_index
            };
            self.duration_bytes += encoded_len(index_delta as u64) + encoded_len(word.duration_ns);
            self.duration_count += 1;
            self.last_duration_index = index;
        }
        self.max_value = self.max_value.max(word.value);
        self.words.push(word);
    }
}

impl Default for WordBlockBuilder {
    fn default() -> Self {
        Self::new(BlockCodecConfig::default()).expect("default block codec configuration is valid")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedBlockMetadata {
    pub header: WordBlockHeader,
    pub restarts: Vec<RestartEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedWordBlock {
    pub header: WordBlockHeader,
    pub restarts: Vec<RestartEntry>,
    pub words: Vec<Word>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedWordRange {
    pub header: WordBlockHeader,
    pub words: Vec<Word>,
    pub complete: bool,
    pub decoded_records: usize,
}

fn encode_word_block_with_interval(
    sequence: u64,
    words: &[Word],
    restart_interval: usize,
    output: &mut Vec<u8>,
) -> CodecResult<EncodedBlockMetadata> {
    if words.is_empty() {
        return Err(CodecError::EmptyBlock);
    }
    if restart_interval == 0 {
        return Err(CodecError::InvalidRestartInterval);
    }
    if words.len() > u32::MAX as usize {
        return Err(CodecError::TooManyWords(words.len()));
    }
    validate_order(words)?;

    let value_bytes = value_width(words.iter().map(|word| word.value).max().unwrap());
    let mut records = Vec::with_capacity(words.len() * (value_bytes + 1));
    let mut durations = Vec::new();
    let mut restarts = Vec::with_capacity(words.len().div_ceil(restart_interval));
    let mut previous_timestamp = words[0].timestamp_ns;
    let mut previous_duration_index = 0usize;
    let mut duration_count = 0usize;

    for (index, word) in words.iter().enumerate() {
        if index.is_multiple_of(restart_interval) {
            restarts.push(RestartEntry {
                timestamp_ns: word.timestamp_ns,
                payload_offset: u32::try_from(records.len()).map_err(|_| {
                    CodecError::InvalidFormat("record payload exceeds 4 GiB".to_string())
                })?,
                record_index: index as u32,
            });
        }
        let delta = if index == 0 {
            0
        } else {
            word.timestamp_ns - previous_timestamp
        };
        encode_u64(delta, &mut records);
        append_value(word.value, value_bytes, &mut records);
        previous_timestamp = word.timestamp_ns;

        if word.duration_ns != 0 {
            let index_delta = if duration_count == 0 {
                index
            } else {
                index - previous_duration_index
            };
            encode_u64(index_delta as u64, &mut durations);
            encode_u64(word.duration_ns, &mut durations);
            previous_duration_index = index;
            duration_count += 1;
        }
    }

    let restart_table_offset = BLOCK_HEADER_SIZE
        .checked_add(records.len())
        .ok_or_else(|| invalid("word-block size overflow"))?;
    let duration_table_offset = restart_table_offset
        .checked_add(restarts.len() * RESTART_ENTRY_SIZE)
        .ok_or_else(|| invalid("word-block size overflow"))?;
    let unpadded_len = duration_table_offset
        .checked_add(durations.len())
        .ok_or_else(|| invalid("word-block size overflow"))?;
    let block_len = unpadded_len
        .checked_add(7)
        .map(|length| length & !7)
        .ok_or_else(|| invalid("word-block size overflow"))?;

    let mut header = WordBlockHeader {
        flags: if duration_count == 0 {
            0
        } else {
            BLOCK_FLAG_HAS_DURATIONS
        },
        sequence,
        first_timestamp_ns: words[0].timestamp_ns,
        last_timestamp_ns: words.last().unwrap().timestamp_ns,
        word_count: words.len() as u32,
        value_bytes: value_bytes as u8,
        record_payload_len: to_u32(records.len(), "record payload")?,
        restart_count: to_u32(restarts.len(), "restart table")?,
        restart_table_offset: to_u32(restart_table_offset, "restart table offset")?,
        duration_count: to_u32(duration_count, "duration table")?,
        duration_table_offset: to_u32(duration_table_offset, "duration table offset")?,
        block_len: to_u32(block_len, "word block")?,
        crc32c: 0,
    };

    output.clear();
    output.resize(BLOCK_HEADER_SIZE, 0);
    output.extend_from_slice(&records);
    for restart in &restarts {
        restart.append_to(output);
    }
    output.extend_from_slice(&durations);
    output.resize(block_len, 0);
    header.write_to(output);
    header.crc32c = block_checksum(output, BLOCK_CHECKSUM_OFFSET);
    header.write_to(output);

    Ok(EncodedBlockMetadata { header, restarts })
}

pub fn decode_word_block(bytes: &[u8]) -> CodecResult<DecodedWordBlock> {
    let parsed = parse_word_block(bytes)?;
    let header = parsed.header;
    let value_bytes = parsed.value_bytes;
    let record_end = parsed.record_end;

    let mut cursor = BLOCK_HEADER_SIZE;
    let mut words = Vec::with_capacity(header.word_count as usize);
    let mut timestamp = header.first_timestamp_ns;
    let mut restart_index = 0usize;
    for record_index in 0..header.word_count as usize {
        let payload_offset = (cursor - BLOCK_HEADER_SIZE) as u32;
        let delta = decode_u64(&bytes[..record_end], &mut cursor)?;
        if record_index == 0 {
            if delta != 0 {
                return Err(invalid("first timestamp delta is not zero"));
            }
        } else {
            timestamp = timestamp
                .checked_add(delta)
                .ok_or_else(|| invalid("timestamp delta overflow"))?;
        }
        let value = read_value(bytes, &mut cursor, record_end, value_bytes)?;
        words.push(Word::new(value, timestamp));

        if parsed
            .restarts
            .get(restart_index)
            .is_some_and(|restart| restart.record_index as usize == record_index)
        {
            let restart = parsed.restarts[restart_index];
            if restart.timestamp_ns != timestamp || restart.payload_offset != payload_offset {
                return Err(invalid("restart entry does not match record payload"));
            }
            restart_index += 1;
        }
    }
    if cursor != record_end || restart_index != parsed.restarts.len() {
        return Err(invalid("record payload length is inconsistent"));
    }
    if timestamp != header.last_timestamp_ns {
        return Err(invalid("last timestamp does not match block header"));
    }

    apply_durations(bytes, &parsed, 0, &mut words)?;
    let restarts = parsed.restarts;

    Ok(DecodedWordBlock {
        header,
        restarts,
        words,
    })
}

/// Decodes only the records needed around a time window, beginning at the
/// nearest restart entry rather than at the start of the block. The result
/// includes two predecessors and one successor when available. Two prior
/// timestamps are required to infer the cadence before a long word gap.
pub fn decode_word_block_range(
    bytes: &[u8],
    start_ns: u64,
    end_ns: u64,
    max_context_words: usize,
) -> CodecResult<DecodedWordRange> {
    if start_ns > end_ns {
        return Err(invalid("range start is after range end"));
    }
    if max_context_words == 0 {
        return Err(CodecError::InvalidConfiguration(
            "max_context_words must be greater than zero",
        ));
    }
    let parsed = parse_word_block(bytes)?;
    // Start one restart before an exact match so the predecessor that closes
    // at the query boundary is available to the renderer.
    let restart_index = parsed
        .restarts
        .partition_point(|restart| restart.timestamp_ns < start_ns)
        .saturating_sub(1);
    let restart = parsed.restarts[restart_index];
    let mut cursor = BLOCK_HEADER_SIZE + restart.payload_offset as usize;
    let mut timestamp = restart.timestamp_ns;
    let mut previous_predecessor = None;
    let mut predecessor = None;
    let mut selected: Vec<(usize, Word)> = Vec::new();
    let mut decoded_records = 0usize;
    let mut complete = true;

    for record_index in restart.record_index as usize..parsed.header.word_count as usize {
        let delta = decode_u64(&bytes[..parsed.record_end], &mut cursor)?;
        if record_index == restart.record_index as usize {
            if record_index == 0 && delta != 0 {
                return Err(invalid("first timestamp delta is not zero"));
            }
        } else {
            timestamp = timestamp
                .checked_add(delta)
                .ok_or_else(|| invalid("timestamp delta overflow"))?;
        }
        let value = read_value(bytes, &mut cursor, parsed.record_end, parsed.value_bytes)?;
        decoded_records += 1;
        let word = Word::new(value, timestamp);
        if timestamp < start_ns {
            previous_predecessor = predecessor;
            predecessor = Some((record_index, word));
            continue;
        }
        if selected.is_empty() {
            if let Some(previous) = previous_predecessor.take() {
                selected.push(previous);
            }
            if let Some(previous) = predecessor.take()
                && selected.len() < max_context_words
            {
                selected.push(previous);
            }
        }
        if selected.len() >= max_context_words {
            complete = false;
            break;
        }
        selected.push((record_index, word));
        if timestamp > end_ns {
            break;
        }
    }
    if selected.is_empty() {
        if let Some(previous) = previous_predecessor {
            selected.push(previous);
        }
        if let Some(previous) = predecessor
            && selected.len() < max_context_words
        {
            selected.push(previous);
        }
    }

    let first_record_index = selected.first().map_or(0, |(index, _)| *index);
    let mut words: Vec<_> = selected.into_iter().map(|(_, word)| word).collect();
    apply_durations(bytes, &parsed, first_record_index, &mut words)?;
    Ok(DecodedWordRange {
        header: parsed.header,
        words,
        complete,
        decoded_records,
    })
}

struct ParsedWordBlock {
    header: WordBlockHeader,
    restarts: Vec<RestartEntry>,
    record_end: usize,
    duration_offset: usize,
    block_len: usize,
    value_bytes: usize,
}

fn parse_word_block(bytes: &[u8]) -> CodecResult<ParsedWordBlock> {
    let header = WordBlockHeader::from_bytes(bytes)?;
    let block_len = header.block_len as usize;
    if block_len != bytes.len() || block_len < BLOCK_HEADER_SIZE {
        return Err(invalid("word-block length does not match its header"));
    }
    let actual_checksum = block_checksum(bytes, BLOCK_CHECKSUM_OFFSET);
    if actual_checksum != header.crc32c {
        return Err(CodecError::ChecksumMismatch {
            expected: header.crc32c,
            actual: actual_checksum,
        });
    }
    if header.word_count == 0 {
        return Err(CodecError::EmptyBlock);
    }
    let value_bytes = header.value_bytes as usize;
    if !matches!(value_bytes, 1 | 2 | 4 | 8) {
        return Err(invalid("invalid value width"));
    }

    let record_end = BLOCK_HEADER_SIZE
        .checked_add(header.record_payload_len as usize)
        .ok_or_else(|| invalid("record payload offset overflow"))?;
    let restart_offset = header.restart_table_offset as usize;
    let restart_bytes = (header.restart_count as usize)
        .checked_mul(RESTART_ENTRY_SIZE)
        .ok_or_else(|| invalid("restart table size overflow"))?;
    let restart_end = restart_offset
        .checked_add(restart_bytes)
        .ok_or_else(|| invalid("restart table offset overflow"))?;
    let duration_offset = header.duration_table_offset as usize;
    if restart_offset != record_end || duration_offset != restart_end || duration_offset > block_len
    {
        return Err(invalid("word-block table offsets are inconsistent"));
    }

    let mut restarts = Vec::with_capacity(header.restart_count as usize);
    for index in 0..header.restart_count as usize {
        restarts.push(RestartEntry::read_from(
            bytes,
            restart_offset + index * RESTART_ENTRY_SIZE,
        )?);
    }
    validate_restart_order(&restarts, header.word_count, header.record_payload_len)?;
    Ok(ParsedWordBlock {
        header,
        restarts,
        record_end,
        duration_offset,
        block_len,
        value_bytes,
    })
}

fn apply_durations(
    bytes: &[u8],
    parsed: &ParsedWordBlock,
    first_record_index: usize,
    words: &mut [Word],
) -> CodecResult<()> {
    let mut duration_cursor = parsed.duration_offset;
    let mut previous_duration_index = 0usize;
    for exception_index in 0..parsed.header.duration_count as usize {
        let index_delta = decode_u64(bytes, &mut duration_cursor)?;
        let record_index = if exception_index == 0 {
            usize::try_from(index_delta).map_err(|_| invalid("duration index overflow"))?
        } else {
            if index_delta == 0 {
                return Err(invalid("duration exception indices are not increasing"));
            }
            previous_duration_index
                .checked_add(
                    usize::try_from(index_delta).map_err(|_| invalid("duration index overflow"))?,
                )
                .ok_or_else(|| invalid("duration index overflow"))?
        };
        let duration_ns = decode_u64(bytes, &mut duration_cursor)?;
        if duration_ns == 0 {
            return Err(invalid("zero duration stored as an exception"));
        }
        if record_index >= parsed.header.word_count as usize {
            return Err(invalid("duration exception index is out of bounds"));
        }
        if let Some(local_index) = record_index.checked_sub(first_record_index)
            && let Some(word) = words.get_mut(local_index)
        {
            word.duration_ns = duration_ns;
        }
        previous_duration_index = record_index;
    }
    let padding = bytes
        .get(duration_cursor..parsed.block_len)
        .ok_or(CodecError::Truncated)?;
    if padding.len() > 7 || padding.iter().any(|&byte| byte != 0) {
        return Err(invalid("invalid word-block padding"));
    }

    Ok(())
}

fn validate_order(words: &[Word]) -> CodecResult<()> {
    for (index, pair) in words.windows(2).enumerate() {
        if pair[1].timestamp_ns < pair[0].timestamp_ns {
            return Err(CodecError::OutOfOrder {
                index: index + 1,
                previous_timestamp_ns: pair[0].timestamp_ns,
                timestamp_ns: pair[1].timestamp_ns,
            });
        }
    }
    Ok(())
}

fn validate_restart_order(
    restarts: &[RestartEntry],
    word_count: u32,
    payload_len: u32,
) -> CodecResult<()> {
    if restarts.is_empty() || restarts[0].record_index != 0 || restarts[0].payload_offset != 0 {
        return Err(invalid("restart table does not begin at the first record"));
    }
    for pair in restarts.windows(2) {
        if pair[1].record_index <= pair[0].record_index
            || pair[1].payload_offset <= pair[0].payload_offset
            || pair[1].timestamp_ns < pair[0].timestamp_ns
        {
            return Err(invalid("restart entries are not strictly ordered"));
        }
    }
    if restarts.last().is_some_and(|restart| {
        restart.record_index >= word_count || restart.payload_offset >= payload_len
    }) {
        return Err(invalid("restart entry is outside the record payload"));
    }
    Ok(())
}

fn value_width(max_value: u64) -> usize {
    match max_value {
        0..=0xff => 1,
        0x100..=0xffff => 2,
        0x1_0000..=0xffff_ffff => 4,
        _ => 8,
    }
}

fn append_value(value: u64, width: usize, output: &mut Vec<u8>) {
    output.extend_from_slice(&value.to_le_bytes()[..width]);
}

fn read_value(
    bytes: &[u8],
    cursor: &mut usize,
    record_end: usize,
    width: usize,
) -> CodecResult<u64> {
    let end = cursor.checked_add(width).ok_or(CodecError::Truncated)?;
    let encoded = bytes
        .get(*cursor..end.min(record_end))
        .filter(|encoded| encoded.len() == width)
        .ok_or(CodecError::Truncated)?;
    let mut value = [0u8; 8];
    value[..width].copy_from_slice(encoded);
    *cursor = end;
    Ok(u64::from_le_bytes(value))
}

fn to_u32(value: usize, what: &str) -> CodecResult<u32> {
    u32::try_from(value).map_err(|_| invalid(&format!("{what} exceeds 4 GiB")))
}

fn invalid(message: &str) -> CodecError {
    CodecError::InvalidFormat(message.to_string())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    const DEFAULT_RESTART_INTERVAL: usize = 512;

    fn encode_word_block(
        sequence: u64,
        words: &[Word],
        output: &mut Vec<u8>,
    ) -> CodecResult<EncodedBlockMetadata> {
        encode_word_block_with_interval(sequence, words, DEFAULT_RESTART_INTERVAL, output)
    }

    /// Finds the first restart at an equal timestamp, or the last restart
    /// before the requested timestamp.
    fn find_restart_for_timestamp(
        restarts: &[RestartEntry],
        timestamp_ns: u64,
    ) -> Option<RestartEntry> {
        let first_not_less = restarts.partition_point(|entry| entry.timestamp_ns < timestamp_ns);
        if restarts
            .get(first_not_less)
            .is_some_and(|entry| entry.timestamp_ns == timestamp_ns)
        {
            return Some(restarts[first_not_less]);
        }
        first_not_less.checked_sub(1).map(|index| restarts[index])
    }

    fn round_trip(words: &[Word]) -> (EncodedBlockMetadata, Vec<u8>) {
        let mut bytes = Vec::new();
        let metadata = encode_word_block(17, words, &mut bytes).unwrap();
        let decoded = decode_word_block(&bytes).unwrap();
        assert_eq!(decoded.header, metadata.header);
        assert_eq!(decoded.restarts, metadata.restarts);
        assert_eq!(decoded.words, words);
        (metadata, bytes)
    }

    #[test]
    fn block_round_trips_widths_equal_timestamps_and_durations() {
        let words = [
            Word::new(0xff, 100),
            Word::spanning(0x100, 100, 25),
            Word::new(0xffff_ffff, 180),
            Word::spanning(u64::MAX, 1_000_000, u64::MAX),
        ];
        let (metadata, _) = round_trip(&words);
        assert_eq!(metadata.header.value_bytes, 8);
        assert_eq!(metadata.header.duration_count, 2);
    }

    #[test]
    fn value_width_uses_the_narrowest_supported_representation() {
        for (value, expected_width) in [
            (0xff, 1),
            (0x100, 2),
            (0xffff, 2),
            (0x1_0000, 4),
            (0xffff_ffff, 4),
            (0x1_0000_0000, 8),
        ] {
            let (metadata, _) = round_trip(&[Word::new(value, 0)]);
            assert_eq!(metadata.header.value_bytes, expected_width);
        }
    }

    #[test]
    fn randomized_ordered_words_round_trip() {
        let mut random = 0x6a09_e667_f3bc_c909u64;
        for case in 0..64 {
            let count = 1 + (next_random(&mut random) as usize % 2_000);
            let mut timestamp = next_random(&mut random) % 10_000;
            let mut words = Vec::with_capacity(count);
            for index in 0..count {
                timestamp = timestamp.saturating_add(next_random(&mut random) % 1_000);
                let value = match case % 4 {
                    0 => next_random(&mut random) & 0xff,
                    1 => next_random(&mut random) & 0xffff,
                    2 => next_random(&mut random) & 0xffff_ffff,
                    _ => next_random(&mut random),
                };
                let duration = if index % 17 == 0 {
                    next_random(&mut random) % 10_000 + 1
                } else {
                    0
                };
                words.push(Word::spanning(value, timestamp, duration));
            }
            round_trip(&words);
        }
    }

    #[test]
    fn dense_eight_bit_payload_is_at_most_two_point_two_bytes_per_word() {
        let words: Vec<_> = (0..DEFAULT_MAX_WORDS_PER_BLOCK)
            .map(|index| Word::new((index & 0xff) as u64, index as u64 * 80))
            .collect();
        let (metadata, _) = round_trip(&words);
        let bytes_per_word = metadata.header.record_payload_len as f64 / words.len() as f64;
        assert!(bytes_per_word <= 2.2, "{bytes_per_word} bytes/word");
    }

    #[test]
    fn restart_entries_bound_forward_decode_distance() {
        let words: Vec<_> = (0..1_000)
            .map(|index| Word::new(index as u64, index as u64 * 10))
            .collect();
        let (metadata, _) = round_trip(&words);
        assert_eq!(
            metadata.restarts.len(),
            words.len().div_ceil(DEFAULT_RESTART_INTERVAL)
        );
        assert_eq!(
            metadata.restarts[1].record_index as usize,
            DEFAULT_RESTART_INTERVAL
        );
    }

    #[test]
    fn range_decode_starts_at_restart_and_keeps_boundary_context() {
        let words: Vec<_> = (0..2_000)
            .map(|index| {
                if index == 1_505 {
                    Word::spanning(index as u64, index as u64 * 10, 7)
                } else {
                    Word::new(index as u64, index as u64 * 10)
                }
            })
            .collect();
        let mut bytes = Vec::new();
        encode_word_block(0, &words, &mut bytes).unwrap();

        let range = decode_word_block_range(&bytes, 15_000, 15_100, 32).unwrap();
        assert!(range.complete);
        assert_eq!(range.words, words[1_498..=1_511]);
        assert!(
            range.decoded_records <= DEFAULT_RESTART_INTERVAL + 12,
            "decoded {} records",
            range.decoded_records
        );

        let boundary = decode_word_block_range(&bytes, 15_360, 15_370, 8).unwrap();
        assert_eq!(boundary.words, words[1_534..=1_538]);
        assert!(boundary.decoded_records <= DEFAULT_RESTART_INTERVAL + 3);
    }

    #[test]
    fn restart_search_preserves_words_at_duplicate_query_timestamps() {
        let restarts = [
            RestartEntry {
                timestamp_ns: 10,
                payload_offset: 0,
                record_index: 0,
            },
            RestartEntry {
                timestamp_ns: 20,
                payload_offset: 100,
                record_index: 256,
            },
            RestartEntry {
                timestamp_ns: 20,
                payload_offset: 200,
                record_index: 512,
            },
            RestartEntry {
                timestamp_ns: 30,
                payload_offset: 300,
                record_index: 768,
            },
        ];

        assert_eq!(find_restart_for_timestamp(&restarts, 9), None);
        assert_eq!(find_restart_for_timestamp(&restarts, 10), Some(restarts[0]));
        assert_eq!(find_restart_for_timestamp(&restarts, 20), Some(restarts[1]));
        assert_eq!(find_restart_for_timestamp(&restarts, 25), Some(restarts[2]));
        assert_eq!(find_restart_for_timestamp(&restarts, 30), Some(restarts[3]));
    }

    #[test]
    fn builder_reports_word_gap_and_payload_boundaries_without_consuming_word() {
        let mut builder = WordBlockBuilder::new(BlockCodecConfig {
            max_words: 2,
            max_inter_word_gap_ns: 100,
            ..BlockCodecConfig::default()
        })
        .unwrap();
        assert_eq!(builder.push(Word::new(1, 0)).unwrap(), PushResult::Appended);
        assert_eq!(
            builder.push(Word::new(2, 10)).unwrap(),
            PushResult::Appended
        );
        assert_eq!(
            builder.push(Word::new(3, 20)).unwrap(),
            PushResult::BlockFull
        );
        assert_eq!(builder.len(), 2);

        builder.clear();
        assert_eq!(builder.push(Word::new(1, 0)).unwrap(), PushResult::Appended);
        assert_eq!(
            builder.push(Word::new(2, 101)).unwrap(),
            PushResult::BlockFull
        );
        assert_eq!(builder.words(), &[Word::new(1, 0)]);
    }

    #[test]
    fn encoding_rejects_empty_and_out_of_order_blocks() {
        let mut bytes = Vec::new();
        assert_eq!(
            encode_word_block(0, &[], &mut bytes),
            Err(CodecError::EmptyBlock)
        );
        assert!(matches!(
            encode_word_block(0, &[Word::new(0, 2), Word::new(0, 1)], &mut bytes),
            Err(CodecError::OutOfOrder { index: 1, .. })
        ));
    }

    #[test]
    fn decoder_rejects_corruption_and_truncation() {
        let words = [Word::new(7, 10), Word::spanning(8, 20, 3)];
        let (_, mut bytes) = round_trip(&words);
        bytes[BLOCK_HEADER_SIZE + 1] ^= 0x80;
        assert!(matches!(
            decode_word_block(&bytes),
            Err(CodecError::ChecksumMismatch { .. })
        ));

        bytes.pop();
        assert!(decode_word_block(&bytes).is_err());
    }

    #[test]
    fn decoder_validates_restart_structure_after_checksum() {
        let words: Vec<_> = (0..DEFAULT_RESTART_INTERVAL + 44)
            .map(|index| Word::new(index as u64, index as u64 * 10))
            .collect();
        let (metadata, mut bytes) = round_trip(&words);
        let second_restart_index_offset =
            metadata.header.restart_table_offset as usize + RESTART_ENTRY_SIZE + 12;
        bytes[second_restart_index_offset..second_restart_index_offset + 4]
            .copy_from_slice(&0u32.to_le_bytes());
        bytes[BLOCK_CHECKSUM_OFFSET..BLOCK_CHECKSUM_OFFSET + 4].fill(0);
        let checksum = block_checksum(&bytes, BLOCK_CHECKSUM_OFFSET);
        bytes[BLOCK_CHECKSUM_OFFSET..BLOCK_CHECKSUM_OFFSET + 4]
            .copy_from_slice(&checksum.to_le_bytes());

        assert!(matches!(
            decode_word_block(&bytes),
            Err(CodecError::InvalidFormat(_))
        ));
    }

    #[test]
    #[ignore = "release-only throughput guard; run with cargo test --release encode_throughput"]
    fn encode_throughput_exceeds_twenty_million_words_per_second() {
        let words: Vec<_> = (0..DEFAULT_MAX_WORDS_PER_BLOCK)
            .map(|index| Word::new((index & 0xff) as u64, index as u64 * 80))
            .collect();
        let mut output = Vec::new();
        let iterations = 64;
        let start = Instant::now();
        for sequence in 0..iterations {
            encode_word_block(sequence, &words, &mut output).unwrap();
        }
        let throughput = words.len() as f64 * iterations as f64 / start.elapsed().as_secs_f64();
        eprintln!("derived-word codec: {throughput:.1} words/s");
        assert!(
            throughput >= 20_000_000.0,
            "encoded {throughput:.1} words/s"
        );
    }

    fn next_random(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }
}
