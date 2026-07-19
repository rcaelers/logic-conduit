use super::WordPresenceBucket;

const FAN_OUT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WordSummaryRecord {
    pub start_ns: u64,
    pub end_ns: u64,
    pub word_count: u64,
    pub first_block: u64,
    pub block_count: u32,
}

/// A 64-way append-only mipmap whose leaves summarize occupied word runs.
#[derive(Debug, Clone)]
pub(super) struct WordPresenceIndex {
    pub(super) levels: Vec<Vec<WordSummaryRecord>>,
    extent_end_ns: Option<u64>,
    pub(super) prefix_max_end_ns: Vec<u64>,
    prefix_word_counts: Vec<u64>,
}

impl Default for WordPresenceIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl WordPresenceIndex {
    pub(super) fn new() -> Self {
        Self {
            levels: vec![Vec::new()],
            extent_end_ns: None,
            prefix_max_end_ns: Vec::new(),
            prefix_word_counts: vec![0],
        }
    }

    pub(super) fn extent_end_ns(&self) -> Option<u64> {
        self.extent_end_ns
    }

    pub(super) fn push(&mut self, record: WordSummaryRecord) {
        debug_assert!(record.word_count > 0);
        debug_assert!(record.start_ns <= record.end_ns);
        self.extent_end_ns = Some(
            self.extent_end_ns
                .map_or(record.end_ns, |end_ns| end_ns.max(record.end_ns)),
        );
        self.prefix_max_end_ns.push(
            self.prefix_max_end_ns
                .last()
                .copied()
                .map_or(record.end_ns, |end_ns| end_ns.max(record.end_ns)),
        );
        self.prefix_word_counts.push(
            self.prefix_word_counts
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_add(record.word_count),
        );
        self.levels[0].push(record);

        let mut level = 0;
        while self.levels[level].len().is_multiple_of(FAN_OUT) {
            let records = &self.levels[level];
            let combined = combine(&records[records.len() - FAN_OUT..]);
            level += 1;
            if self.levels.len() == level {
                self.levels.push(Vec::new());
            }
            self.levels[level].push(combined);
        }
    }

    pub(super) fn presence_window_all(
        &self,
        start_ns: u64,
        end_ns: u64,
        target_buckets: usize,
    ) -> Vec<WordPresenceBucket> {
        if target_buckets == 0 || start_ns > end_ns {
            return Vec::new();
        }
        let span = end_ns.saturating_sub(start_ns).saturating_add(1);
        let bucket_count = target_buckets
            .min(usize::try_from(span).unwrap_or(usize::MAX))
            .max(1);
        let leaves = &self.levels[0];
        let mut buckets = Vec::with_capacity(bucket_count);

        for bucket_index in 0..bucket_count {
            let bucket_start = start_ns.saturating_add(scale(span, bucket_index, bucket_count));
            let mut bucket_end_exclusive =
                start_ns.saturating_add(scale(span, bucket_index + 1, bucket_count));
            if bucket_index + 1 == bucket_count {
                bucket_end_exclusive = end_ns.saturating_add(1);
            }
            bucket_end_exclusive = bucket_end_exclusive.max(bucket_start.saturating_add(1));

            let first_by_start = leaves.partition_point(|record| record.start_ns < bucket_start);
            let mut first = first_by_start.saturating_sub(1);
            while first > 0 && leaves[first - 1].end_ns >= bucket_start {
                first -= 1;
            }
            let end = leaves.partition_point(|record| record.start_ns < bucket_end_exclusive);
            let word_count =
                self.count_bucket(first.min(end), end, bucket_start, bucket_end_exclusive);
            buckets.push(WordPresenceBucket {
                start_ns: bucket_start,
                end_ns: bucket_end_exclusive.saturating_sub(1),
                word_count,
            });
        }
        buckets
    }

    fn count_bucket(
        &self,
        first: usize,
        end: usize,
        bucket_start: u64,
        bucket_end_exclusive: u64,
    ) -> u64 {
        if first >= end {
            return 0;
        }
        let leaves = &self.levels[0];
        let mut full_start = first;
        let mut count = 0u64;
        while full_start < end
            && (leaves[full_start].start_ns < bucket_start
                || leaves[full_start].end_ns >= bucket_end_exclusive)
        {
            count = count.saturating_add(estimate_partial(
                leaves[full_start],
                bucket_start,
                bucket_end_exclusive,
            ));
            full_start += 1;
        }

        let mut full_end = end;
        while full_end > full_start && leaves[full_end - 1].end_ns >= bucket_end_exclusive {
            full_end -= 1;
            count = count.saturating_add(estimate_partial(
                leaves[full_end],
                bucket_start,
                bucket_end_exclusive,
            ));
        }
        count.saturating_add(
            self.prefix_word_counts[full_end].saturating_sub(self.prefix_word_counts[full_start]),
        )
    }

    #[cfg(test)]
    fn level_len(&self, level: usize) -> usize {
        self.levels.get(level).map_or(0, Vec::len)
    }
}

fn combine(records: &[WordSummaryRecord]) -> WordSummaryRecord {
    WordSummaryRecord {
        start_ns: records[0].start_ns,
        end_ns: records.iter().map(|record| record.end_ns).max().unwrap(),
        word_count: records.iter().map(|record| record.word_count).sum(),
        first_block: records[0].first_block,
        block_count: records
            .iter()
            .map(|record| u64::from(record.block_count))
            .sum::<u64>()
            .min(u64::from(u32::MAX)) as u32,
    }
}

fn estimate_partial(
    record: WordSummaryRecord,
    bucket_start: u64,
    bucket_end_exclusive: u64,
) -> u64 {
    let record_end_exclusive = record.end_ns.saturating_add(1);
    let overlap_start = record.start_ns.max(bucket_start);
    let overlap_end = record_end_exclusive.min(bucket_end_exclusive);
    if overlap_start >= overlap_end {
        return 0;
    }
    let record_span = record_end_exclusive.saturating_sub(record.start_ns).max(1);
    let overlap = overlap_end - overlap_start;
    ((u128::from(record.word_count) * u128::from(overlap))
        .div_ceil(u128::from(record_span))
        .min(u128::from(record.word_count))) as u64
}

fn scale(span: u64, numerator: usize, denominator: usize) -> u64 {
    ((u128::from(span) * numerator as u128) / denominator as u128).min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(block: u64, timestamp_ns: u64, count: u64) -> WordSummaryRecord {
        WordSummaryRecord {
            start_ns: timestamp_ns,
            end_ns: timestamp_ns,
            word_count: count,
            first_block: block,
            block_count: 1,
        }
    }

    #[test]
    fn completed_groups_fold_into_64_way_levels() {
        let mut index = WordPresenceIndex::new();
        for block in 0..(FAN_OUT * FAN_OUT + 3) {
            index.push(point(block as u64, block as u64, 1));
        }
        assert_eq!(index.level_len(0), FAN_OUT * FAN_OUT + 3);
        assert_eq!(index.level_len(1), FAN_OUT);
        assert_eq!(index.level_len(2), 1);
        assert_eq!(
            index.levels[0]
                .iter()
                .map(|record| record.word_count)
                .sum::<u64>(),
            (FAN_OUT * FAN_OUT + 3) as u64
        );
    }

    #[test]
    fn sparse_gaps_produce_no_presence_between_leaf_records() {
        let mut index = WordPresenceIndex::new();
        index.push(point(0, 10, 1));
        index.push(point(1, 10_000, 1));
        let mut buckets = index.presence_window_all(0, 10_009, 10);
        buckets.retain(|bucket| bucket.word_count > 0);
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].start_ns, 0);
        assert_eq!(buckets[1].end_ns, 10_009);
    }

    #[test]
    fn extent_keeps_a_long_word_that_ends_after_later_blocks() {
        let mut index = WordPresenceIndex::new();
        index.push(WordSummaryRecord {
            start_ns: 10,
            end_ns: 10_000,
            word_count: 1,
            first_block: 0,
            block_count: 1,
        });
        index.push(point(1, 100, 1));
        assert_eq!(index.extent_end_ns(), Some(10_000));
    }

    #[test]
    fn dense_point_records_match_direct_bucket_counts() {
        let mut index = WordPresenceIndex::new();
        for block in 0..10_000u64 {
            index.push(point(block, block, block % 7 + 1));
        }
        let buckets = index.presence_window_all(0, 9_999, 100);
        for bucket in buckets {
            let expected: u64 = (bucket.start_ns..=bucket.end_ns)
                .map(|timestamp| timestamp % 7 + 1)
                .sum();
            assert_eq!(bucket.word_count, expected);
        }
    }

    #[test]
    fn overview_result_is_bounded_by_target_bucket_count() {
        let mut index = WordPresenceIndex::new();
        for block in 0..100_000u64 {
            index.push(point(block, block * 10, 64));
        }
        assert!(index.presence_window_all(0, 1_000_000, 1_920).len() <= 1_920);
    }
}
