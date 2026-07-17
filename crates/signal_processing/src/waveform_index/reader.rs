use std::path::Path;

use super::builder::IndexBuilder;
use super::exact_window_sample_limit;
use super::storage::{IndexReader, LevelsView};
use super::types::{
    CaptureIndexProgress, SAMPLES_PER_L1_BIT, SAMPLES_PER_L2_BIT, SAMPLES_PER_L3_BIT, bit,
};
use crate::raw_block_cache::RawBlockCache;
use crate::capture::{BlockCaptureSource, BlockData, CaptureDataSource, CaptureIndex, CaptureMetadata, CaptureSampledChannel, CaptureSampledWindow, CaptureTransition, CaptureWaveformSegment, packed_bit};
use crate::{Error, Result};

#[derive(Clone, Copy)]
struct GroupSummary {
    first: bool,
    toggle: bool,
    last: bool,
}

/// Windowed sampler for indexed capture data.
///
/// Handles index construction/loading and samples visible windows from an
/// [`IndexReader`], falling back to a raw reader for deep zoom levels.
pub struct IndexSampler<R: BlockCaptureSource> {
    display_name: String,
    storage: IndexReader,
    raw_reader: R,
    /// Sparse on-disk cache of decompressed raw blocks; optional because the
    /// sampler works (more slowly) without it.
    raw_cache: Option<RawBlockCache>,
}

impl<R> IndexSampler<R>
where
    R: BlockCaptureSource,
{
    pub(super) fn new(
        display_name: String,
        storage: IndexReader,
        raw_reader: R,
        raw_cache: Option<RawBlockCache>,
    ) -> Self {
        Self {
            display_name,
            storage,
            raw_reader,
            raw_cache,
        }
    }

    pub fn open_data_source<S>(data_source: S) -> Result<Self>
    where
        S: CaptureDataSource<Reader = R>,
    {
        Self::open_data_source_with_progress(data_source, |_| {})
    }

    pub fn open_data_source_with_progress<S, C>(data_source: S, progress: C) -> Result<Self>
    where
        S: CaptureDataSource<Reader = R>,
        C: FnMut(CaptureIndexProgress),
    {
        let header = data_source.metadata().clone();
        let fingerprint = data_source.fingerprint();
        let index_path = data_source
            .index_path()
            .ok_or_else(|| Error::ParseError("capture source is not indexable".to_string()))?;

        if !IndexReader::is_valid(&index_path, &header, fingerprint.revision)? {
            IndexBuilder::new(&data_source, &index_path, &header, fingerprint.revision)
                .build(progress)?;
        }

        let raw_cache = RawBlockCache::open(
            &index_path.with_extension("raw"),
            &header,
            fingerprint.revision,
        )
        .ok();
        let storage = IndexReader::open(index_path, header, fingerprint.revision)?;
        let display_name = data_source.display_name();
        let raw_reader = data_source.open_reader()?;
        Ok(Self::new(display_name, storage, raw_reader, raw_cache))
    }

    pub fn display_name(&self) -> String {
        self.display_name.clone()
    }

    pub fn index_path(&self) -> &Path {
        self.storage.path()
    }

    pub fn header(&self) -> &CaptureMetadata {
        self.storage.header()
    }

    pub fn capture_duration_us(&self) -> f64 {
        self.header().total_samples as f64 * 1_000_000.0 / self.header().samplerate_hz
    }

    /// Fraction of 64-sample groups containing one or more transitions.
    /// Reads only the mmap'd waveform index; raw capture blocks and the raw
    /// cache are never touched.
    pub fn activity_ratio_hint(&self, channel: usize, limit: u64) -> Result<f64> {
        if channel >= self.header().total_probes {
            return Err(Error::InvalidProbe(channel));
        }
        let limit = limit.min(self.header().total_samples);
        if limit == 0 {
            return Ok(0.0);
        }

        let samples_per_block = self.header().samples_per_block;
        let blocks = limit.div_ceil(samples_per_block);
        let mut active_groups = 0u64;
        let mut total_groups = 0u64;
        for block in 0..blocks {
            let block_start = block * samples_per_block;
            let valid_samples = (limit - block_start).min(samples_per_block);
            let groups = valid_samples.div_ceil(SAMPLES_PER_L1_BIT) as usize;
            total_groups += groups as u64;

            let root = self.storage.load_root_summary(channel, block as usize)?;
            if !root.toggle {
                continue;
            }
            let leaf = self.storage.load_leaf(channel, block as usize)?;
            let Some(levels) = leaf.levels else {
                continue;
            };
            let full_words = groups / u64::BITS as usize;
            active_groups += levels.l1_toggle[..full_words]
                .iter()
                .map(|word| u64::from(word.count_ones()))
                .sum::<u64>();
            let remainder = groups % u64::BITS as usize;
            if remainder > 0 {
                let mask = (1u64 << remainder) - 1;
                active_groups += u64::from((levels.l1_toggle[full_words] & mask).count_ones());
            }
        }
        Ok(active_groups as f64 / total_groups.max(1) as f64)
    }

    /// Value of `channel` at `position`. O(1) after the containing block is
    /// cached (raw block cache, or the raw reader's own LRU).
    pub fn value_at(&mut self, channel: usize, position: u64) -> Result<bool> {
        if channel >= self.header().total_probes {
            return Err(Error::InvalidProbe(channel));
        }
        if position >= self.header().total_samples {
            return Err(Error::OutOfBounds(position));
        }
        let samples_per_block = self.header().samples_per_block;
        let block = position / samples_per_block;
        let data = self.cached_packed_block(channel, block)?;
        Ok(packed_bit(&data, (position % samples_per_block) as usize))
    }

    /// First transition strictly after `position` and before `limit`.
    ///
    /// The display index is descended from its block summary through L3,
    /// L2, and L1. Only the final 64-sample candidate group touches the raw
    /// packed data. This keeps long constant ranges index-only and avoids
    /// constructing a complete sampled-window transition vector when the
    /// caller needs just one edge.
    pub fn next_transition(
        &mut self,
        channel: usize,
        position: u64,
        limit: u64,
    ) -> Result<Option<CaptureTransition>> {
        if channel >= self.header().total_probes {
            return Err(Error::InvalidProbe(channel));
        }

        let limit = limit.min(self.header().total_samples);
        let Some(mut search) = position.checked_add(1) else {
            return Ok(None);
        };
        if search >= limit {
            return Ok(None);
        }

        let samples_per_block = self.header().samples_per_block;
        while search < limit {
            let block = search / samples_per_block;
            let block_start = block * samples_per_block;
            let block_limit = block_start.saturating_add(samples_per_block).min(limit);
            let local_limit = block_limit - block_start;

            let root = self.storage.load_root_summary(channel, block as usize)?;
            if !root.toggle {
                search = block_limit;
                continue;
            }

            let local_search = search - block_start;
            let candidate = {
                let leaf = self.storage.load_leaf(channel, block as usize)?;
                leaf.levels
                    .as_ref()
                    .and_then(|levels| next_indexed_l1_group(levels, local_search, local_limit))
            };
            let Some(l1_group) = candidate else {
                search = block_limit;
                continue;
            };

            let group_start = l1_group as u64 * SAMPLES_PER_L1_BIT;
            let scan_start = local_search.max(group_start);
            let scan_end = local_limit.min(group_start + SAMPLES_PER_L1_BIT);
            if let Some(transition) =
                self.next_raw_transition(channel, block, scan_start, scan_end)?
            {
                return Ok(Some(transition));
            }

            // The index can mark a group because of a transition at its
            // first sample that is at/before `position`. Once the exact scan
            // proves there is no later edge in the group, skip it entirely.
            search = block_start + scan_end;
        }

        Ok(None)
    }

    /// Appends up to `max_transitions` exact transitions after `position` and
    /// before `limit`, descending the index once per active 64-sample group.
    pub fn next_transitions(
        &mut self,
        channel: usize,
        position: u64,
        limit: u64,
        max_transitions: usize,
        output: &mut Vec<CaptureTransition>,
    ) -> Result<()> {
        output.clear();
        if channel >= self.header().total_probes {
            return Err(Error::InvalidProbe(channel));
        }
        if max_transitions == 0 {
            return Ok(());
        }

        let limit = limit.min(self.header().total_samples);
        let Some(mut search) = position.checked_add(1) else {
            return Ok(());
        };
        if search >= limit {
            return Ok(());
        }

        output.reserve(max_transitions.min(65_536));
        let samples_per_block = self.header().samples_per_block;
        while search < limit && output.len() < max_transitions {
            let block = search / samples_per_block;
            let block_start = block * samples_per_block;
            let block_limit = block_start.saturating_add(samples_per_block).min(limit);
            let local_limit = block_limit - block_start;
            let root = self.storage.load_root_summary(channel, block as usize)?;
            if !root.toggle {
                search = block_limit;
                continue;
            }

            // Acquire one packed block view and one leaf view for every
            // contiguous search through this block. Both are mmap/Arc views;
            // no sample payload is copied here.
            let data = self.cached_packed_block(channel, block)?;
            let leaf = self.storage.load_leaf(channel, block as usize)?;
            let Some(levels) = leaf.levels.as_ref() else {
                search = block_limit;
                continue;
            };
            let previous_block_last = if block > 0 {
                Some(
                    self.storage
                        .load_root_summary(channel, block as usize - 1)?
                        .last,
                )
            } else {
                None
            };

            let mut local_search = search - block_start;
            while local_search < local_limit && output.len() < max_transitions {
                let Some(l1_group) = next_indexed_l1_group(levels, local_search, local_limit)
                else {
                    break;
                };
                let group_start = l1_group as u64 * SAMPLES_PER_L1_BIT;
                let scan_start = local_search.max(group_start);
                let scan_end = local_limit.min(group_start + SAMPLES_PER_L1_BIT);
                append_raw_transitions(
                    &data,
                    block_start,
                    scan_start,
                    scan_end,
                    previous_block_last,
                    max_transitions,
                    output,
                );
                local_search = scan_end;
            }
            search = block_start + local_search;
            if local_search >= local_limit
                || next_indexed_l1_group(levels, local_search, local_limit).is_none()
            {
                search = block_limit;
            }
        }
        Ok(())
    }

    /// Reads a sorted batch of positions with one packed block acquisition
    /// per block instead of one acquisition per sample.
    pub fn values_at(
        &mut self,
        channel: usize,
        positions: &[u64],
        output: &mut Vec<bool>,
    ) -> Result<()> {
        if channel >= self.header().total_probes {
            return Err(Error::InvalidProbe(channel));
        }
        output.clear();
        output.reserve(positions.len());

        let samples_per_block = self.header().samples_per_block;
        let mut cursor = 0;
        while cursor < positions.len() {
            let position = positions[cursor];
            if position >= self.header().total_samples {
                return Err(Error::OutOfBounds(position));
            }
            let block = position / samples_per_block;
            let data = self.cached_packed_block(channel, block)?;
            while cursor < positions.len() {
                let position = positions[cursor];
                if position >= self.header().total_samples {
                    return Err(Error::OutOfBounds(position));
                }
                if position / samples_per_block != block {
                    break;
                }
                output.push(packed_bit(&data, (position % samples_per_block) as usize));
                cursor += 1;
            }
        }
        Ok(())
    }

    fn next_raw_transition(
        &mut self,
        channel: usize,
        block: u64,
        local_start: u64,
        local_end: u64,
    ) -> Result<Option<CaptureTransition>> {
        if local_start >= local_end {
            return Ok(None);
        }

        let samples_per_block = self.header().samples_per_block;
        let block_start = block * samples_per_block;
        let data = self.cached_packed_block(channel, block)?;
        let word_index = local_start as usize / 64;
        let word_start = word_index * 64;
        let word = load_le_word(&data, word_index);
        let entering = if word_start > 0 {
            packed_bit(&data, word_start - 1)
        } else if block > 0 {
            self.storage
                .load_root_summary(channel, block as usize - 1)?
                .last
        } else {
            // Sample zero has no predecessor and therefore is not itself a
            // transition. Treat its own value as the entering level.
            word & 1 != 0
        };

        let lo = local_start as usize - word_start;
        let hi = local_end as usize - word_start;
        let mut toggles = word ^ ((word << 1) | entering as u64);
        toggles &= range_mask(lo, hi);
        let Some(bit_index) = nonzero_trailing_bit(toggles) else {
            return Ok(None);
        };

        Ok(Some(CaptureTransition {
            sample: block_start + (word_start + bit_index) as u64,
            value: bit(word, bit_index),
        }))
    }

    pub fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> Result<CaptureSampledWindow> {
        let total_samples = self.header().total_samples;
        let start_sample = start_sample.min(total_samples.saturating_sub(1));
        let end_sample = end_sample.clamp(start_sample + 1, total_samples);
        let samples = end_sample - start_sample;
        let target_points = target_points.max(1);
        let target_points_u64 = target_points as u64;
        let sample_step = samples.div_ceil(target_points_u64).max(1);

        if samples <= exact_window_sample_limit(target_points) {
            return self.exact_sampled_window(channels, start_sample, end_sample);
        }

        let group_samples = if sample_step >= self.header().samples_per_block {
            self.header().samples_per_block
        } else if sample_step >= SAMPLES_PER_L3_BIT {
            SAMPLES_PER_L3_BIT
        } else if sample_step >= SAMPLES_PER_L2_BIT {
            SAMPLES_PER_L2_BIT
        } else {
            SAMPLES_PER_L1_BIT
        };

        let mut sampled_channels = Vec::with_capacity(channels.len());
        for &channel in channels {
            sampled_channels.push(self.sample_indexed_channel(
                channel,
                start_sample,
                end_sample,
                target_points,
                group_samples,
            )?);
        }

        Ok(CaptureSampledWindow {
            start_sample,
            end_sample,
            sample_step: group_samples,
            channels: sampled_channels,
        })
    }

    fn exact_sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
    ) -> Result<CaptureSampledWindow> {
        let mut sampled_channels = Vec::with_capacity(channels.len());
        for &channel in channels {
            sampled_channels.push(self.exact_sampled_channel(channel, start_sample, end_sample)?);
        }

        Ok(CaptureSampledWindow {
            start_sample,
            end_sample,
            sample_step: 1,
            channels: sampled_channels,
        })
    }

    fn exact_sampled_channel(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
    ) -> Result<CaptureSampledChannel> {
        if channel >= self.header().total_probes {
            return Err(Error::InvalidProbe(channel));
        }

        let name = self
            .header()
            .probe_names
            .get(channel)
            .cloned()
            .unwrap_or_else(|| format!("Probe{}", channel));
        let samples_per_block = self.header().samples_per_block;
        let first_block = start_sample / samples_per_block;
        let last_block = (end_sample - 1) / samples_per_block;
        let mut current = {
            let data = self.cached_packed_block(channel, first_block)?;
            packed_bit(&data, (start_sample % samples_per_block) as usize)
        };
        let initial = current;
        let mut transitions = Vec::new();

        for block in first_block..=last_block {
            let data = self.cached_packed_block(channel, block)?;
            let block_start = block * samples_per_block;
            let block_end = block_start
                .saturating_add(samples_per_block)
                .min(end_sample);
            // Transitions are reported from the second window sample onwards.
            let scan_start = block_start.max(start_sample + 1);
            if scan_start >= block_end {
                continue;
            }

            let lo_local = (scan_start - block_start) as usize;
            let hi_local = (block_end - block_start) as usize;
            let first_word = lo_local / 64;
            let last_word = (hi_local - 1) / 64;
            for word_index in first_word..=last_word {
                let word = load_le_word(&data, word_index);
                let lo = if word_index == first_word {
                    lo_local % 64
                } else {
                    0
                };
                let hi = if word_index == last_word {
                    hi_local - word_index * 64
                } else {
                    64
                };

                // Bit i marks a change between sample i and sample i-1; the
                // shifted-in bit 0 compares against `current`, the value of
                // the last sample processed before this word.
                let mut toggles = word ^ ((word << 1) | current as u64);
                toggles &= range_mask(lo, hi);

                while toggles != 0 {
                    let bit_index = toggles.trailing_zeros() as usize;
                    toggles &= toggles - 1;
                    let value = (word >> bit_index) & 1 != 0;
                    transitions.push(CaptureTransition {
                        sample: block_start + (word_index * 64 + bit_index) as u64,
                        value,
                    });
                }
                current = (word >> (hi - 1)) & 1 != 0;
            }
        }

        Ok(CaptureSampledChannel {
            channel,
            name,
            initial,
            transitions,
            waveform: Vec::new(),
        })
    }

    /// Packed block bytes, preferring the sparse raw cache over the (usually
    /// compressed) capture source; freshly decompressed blocks are stored in
    /// the cache for later zero-copy reads.
    fn cached_packed_block(&mut self, channel: usize, block: u64) -> Result<BlockData> {
        if let Some(data) = self
            .raw_cache
            .as_ref()
            .and_then(|cache| cache.get(channel, block))
        {
            return Ok(data);
        }
        let data = self.raw_reader.read_packed_block(channel, block)?;
        if let Some(cache) = self.raw_cache.as_mut() {
            cache.put(channel, block, &data);
        }
        Ok(data)
    }

    /// Returns one packed capture block for an external streaming source.
    ///
    /// Existing raw-cache entries are reused, but a miss is read directly
    /// from the capture source without populating the cache. Sequential graph
    /// processing may visit the complete capture, whereas the raw cache is
    /// intentionally sparse and reserved for regions inspected through
    /// random-access queries.
    pub fn packed_block(&mut self, channel: usize, block: u64) -> Result<BlockData> {
        if let Some(data) = self
            .raw_cache
            .as_ref()
            .and_then(|cache| cache.get(channel, block))
        {
            return Ok(data);
        }
        self.raw_reader.read_packed_block(channel, block)
    }

    fn sample_indexed_channel(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
        group_samples: u64,
    ) -> Result<CaptureSampledChannel> {
        if channel >= self.header().total_probes {
            return Err(Error::InvalidProbe(channel));
        }

        let name = self
            .header()
            .probe_names
            .get(channel)
            .cloned()
            .unwrap_or_else(|| format!("Probe{}", channel));
        let initial = self.indexed_initial_value(channel, start_sample, group_samples)?;
        let transitions = Vec::new();
        let mut waveform = Vec::new();

        let samples = end_sample - start_sample;
        let target_points = target_points.max(1) as u64;
        let mut previous_end = start_sample;
        let mut previous_value = initial;

        for point in 0..target_points {
            let visible_start = start_sample + samples.saturating_mul(point) / target_points;
            let visible_end = if point + 1 == target_points {
                end_sample
            } else {
                start_sample + samples.saturating_mul(point + 1) / target_points
            };
            if visible_end <= visible_start || visible_start < previous_end {
                continue;
            }
            previous_end = visible_end;

            let summary = self.indexed_display_range_summary(
                channel,
                visible_start,
                visible_end,
                group_samples,
                previous_value,
            )?;
            self.append_pixel_waveform(
                visible_start,
                visible_end,
                summary,
                &mut previous_value,
                &mut waveform,
            );
        }

        Ok(CaptureSampledChannel {
            channel,
            name,
            initial,
            transitions,
            waveform,
        })
    }

    /// Group-aligned value entering `sample`, derived purely from the index.
    /// Keeps the indexed path free of raw-capture reads (which decompress
    /// whole blocks); consistent with the per-pixel summaries, which are
    /// aligned to the same groups.
    fn indexed_initial_value(
        &mut self,
        channel: usize,
        sample: u64,
        group_samples: u64,
    ) -> Result<bool> {
        let block = self.block_for_sample(sample);
        let local = sample % self.header().samples_per_block;
        Ok(self
            .block_local_display_summary(channel, block as usize, local, local + 1, group_samples)?
            .first)
    }

    fn indexed_display_range_summary(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
        group_samples: u64,
        fallback_first: bool,
    ) -> Result<GroupSummary> {
        let mut first = None;
        let mut last = fallback_first;
        let mut toggle = false;
        let samples_per_block = self.header().samples_per_block;
        let first_block = self.block_for_sample(start_sample);
        let last_block = self.block_for_sample(end_sample.saturating_sub(1));

        for block in first_block..=last_block {
            let block_start = block * samples_per_block;
            let local_start = start_sample.saturating_sub(block_start);
            let local_end = end_sample
                .saturating_sub(block_start)
                .min(samples_per_block);
            if local_end <= local_start {
                continue;
            }

            let summary = self.block_local_display_summary(
                channel,
                block as usize,
                local_start,
                local_end,
                group_samples,
            )?;
            let range_first = *first.get_or_insert(summary.first);
            toggle |= summary.toggle || summary.first != last || range_first != summary.last;
            last = summary.last;
        }

        Ok(GroupSummary {
            first: first.unwrap_or(fallback_first),
            toggle,
            last,
        })
    }

    fn block_local_display_summary(
        &mut self,
        channel: usize,
        block: usize,
        local_start: u64,
        local_end: u64,
        group_samples: u64,
    ) -> Result<GroupSummary> {
        let samples_per_block = self.header().samples_per_block;
        let block_start = block as u64 * samples_per_block;
        let valid_end = self
            .header()
            .total_samples
            .saturating_sub(block_start)
            .min(samples_per_block);

        if local_start == 0 && local_end >= valid_end {
            let entry = self.storage.load_root_summary(channel, block)?;
            return Ok(GroupSummary {
                first: entry.first,
                toggle: entry.toggle,
                last: entry.last,
            });
        }

        if group_samples >= SAMPLES_PER_L3_BIT {
            let entry = self.storage.load_root_summary(channel, block)?;
            // Constant blocks store no level bitmaps; their l3_last/l3_toggle
            // words are zero and must not be interpreted as sample values.
            if !entry.toggle {
                return Ok(GroupSummary {
                    first: entry.first,
                    toggle: false,
                    last: entry.last,
                });
            }
            let first_group = (local_start / SAMPLES_PER_L3_BIT).min(63) as usize;
            let last_group = ((local_end - 1) / SAMPLES_PER_L3_BIT).min(63) as usize;
            let first = if first_group == 0 {
                entry.first
            } else {
                bit(entry.l3_last, first_group - 1)
            };
            let last = bit(entry.l3_last, last_group);
            return Ok(GroupSummary {
                first,
                toggle: bit_range_any(&[entry.l3_toggle], first_group, last_group) || first != last,
                last,
            });
        }

        let leaf = self.storage.load_leaf(channel, block)?;
        let Some(levels) = leaf.levels else {
            return Ok(GroupSummary {
                first: leaf.first,
                toggle: false,
                last: leaf.last,
            });
        };

        if group_samples >= SAMPLES_PER_L2_BIT {
            let first_group = (local_start / SAMPLES_PER_L2_BIT).min(4095) as usize;
            let last_group = ((local_end - 1) / SAMPLES_PER_L2_BIT).min(4095) as usize;
            let first = if first_group == 0 {
                leaf.first
            } else {
                bit(
                    levels.l2_last[(first_group - 1) / 64],
                    (first_group - 1) % 64,
                )
            };
            let last = bit(levels.l2_last[last_group / 64], last_group % 64);
            Ok(GroupSummary {
                first,
                toggle: bit_range_any(levels.l2_toggle, first_group, last_group) || first != last,
                last,
            })
        } else {
            let first_group = (local_start / SAMPLES_PER_L1_BIT).min(262_143) as usize;
            let last_group = ((local_end - 1) / SAMPLES_PER_L1_BIT).min(262_143) as usize;
            let first = if first_group == 0 {
                leaf.first
            } else {
                bit(
                    levels.l1_last[(first_group - 1) / 64],
                    (first_group - 1) % 64,
                )
            };
            let last = bit(levels.l1_last[last_group / 64], last_group % 64);
            Ok(GroupSummary {
                first,
                toggle: bit_range_any(levels.l1_toggle, first_group, last_group) || first != last,
                last,
            })
        }
    }

    fn append_pixel_waveform(
        &self,
        start_sample: u64,
        end_sample: u64,
        summary: GroupSummary,
        previous_value: &mut bool,
        waveform: &mut Vec<CaptureWaveformSegment>,
    ) {
        if end_sample <= start_sample {
            return;
        }

        if summary.toggle {
            push_activity(
                waveform,
                start_sample,
                end_sample,
                *previous_value,
                summary.last,
            );
            *previous_value = summary.last;
            return;
        }

        if summary.first == *previous_value {
            push_level(waveform, start_sample, end_sample, summary.first);
            *previous_value = summary.last;
            return;
        }

        waveform.push(CaptureWaveformSegment::Edge {
            sample: start_sample,
            before: *previous_value,
            after: summary.first,
        });
        push_level(waveform, start_sample, end_sample, summary.first);
        *previous_value = summary.last;
    }

    fn block_for_sample(&self, sample: u64) -> u64 {
        sample / self.header().samples_per_block
    }
}

fn push_level(
    waveform: &mut Vec<CaptureWaveformSegment>,
    start_sample: u64,
    end_sample: u64,
    value: bool,
) {
    if end_sample <= start_sample {
        return;
    }

    if let Some(CaptureWaveformSegment::Level {
        end_sample: previous_end,
        value: previous_value,
        ..
    }) = waveform.last_mut()
        && *previous_end == start_sample
        && *previous_value == value
    {
        *previous_end = end_sample;
        return;
    }

    waveform.push(CaptureWaveformSegment::Level {
        start_sample,
        end_sample,
        value,
    });
}

/// Loads the 64-sample word at `word_index` from LSB-first packed bytes,
/// zero-padding past the end of `data` (callers mask out padded bits).
fn load_le_word(data: &[u8], word_index: usize) -> u64 {
    let byte_start = word_index * 8;
    if let Some(chunk) = data.get(byte_start..byte_start + 8) {
        u64::from_le_bytes(chunk.try_into().expect("chunk is 8 bytes"))
    } else {
        let mut bytes = [0_u8; 8];
        let available = data.len().saturating_sub(byte_start).min(8);
        bytes[..available].copy_from_slice(&data[byte_start..byte_start + available]);
        u64::from_le_bytes(bytes)
    }
}

/// Mask selecting bits `lo..hi` (hi exclusive, hi ≤ 64).
fn range_mask(lo: usize, hi: usize) -> u64 {
    let upper = if hi == 64 {
        u64::MAX
    } else {
        (1_u64 << hi) - 1
    };
    upper & !((1_u64 << lo) - 1)
}

/// Appends an activity range, merging it into a directly adjacent preceding
/// activity segment. Busy regions produce long runs of per-point activity;
/// merging collapses them into one segment per run (`first` stays the value
/// entering the run, `last` tracks the value leaving it), which shrinks the
/// window payload and the per-frame draw work substantially.
fn push_activity(
    waveform: &mut Vec<CaptureWaveformSegment>,
    start_sample: u64,
    end_sample: u64,
    first: bool,
    last: bool,
) {
    if let Some(CaptureWaveformSegment::Activity {
        end_sample: previous_end,
        last: previous_last,
        ..
    }) = waveform.last_mut()
        && *previous_end == start_sample
    {
        *previous_end = end_sample;
        *previous_last = last;
        return;
    }

    waveform.push(CaptureWaveformSegment::Activity {
        start_sample,
        end_sample,
        first,
        last,
    });
}

fn bit_range_any(words: &[u64], first_bit: usize, last_bit: usize) -> bool {
    if last_bit < first_bit {
        return false;
    }

    let first_word = first_bit / 64;
    let last_word = last_bit / 64;
    for word_index in first_word..=last_word {
        let Some(mut word) = words.get(word_index).copied() else {
            break;
        };
        if word_index == first_word {
            word &= u64::MAX << (first_bit % 64);
        }
        if word_index == last_word {
            let end_bit = last_bit % 64;
            let mask = if end_bit == 63 {
                u64::MAX
            } else {
                (1_u64 << (end_bit + 1)) - 1
            };
            word &= mask;
        }
        if word != 0 {
            return true;
        }
    }
    false
}

/// Finds the first active 64-sample L1 group in `start..end` by descending
/// the L3 -> L2 -> L1 toggle summaries. `start` and `end` are block-local
/// sample positions and `end` is exclusive.
fn next_indexed_l1_group(levels: &LevelsView<'_>, start: u64, end: u64) -> Option<usize> {
    if start >= end {
        return None;
    }

    let first_l3 = (start / SAMPLES_PER_L3_BIT) as usize;
    let last_l3 = ((end - 1) / SAMPLES_PER_L3_BIT).min(63) as usize;
    for l3 in first_l3..=last_l3 {
        if !bit(levels.l3_toggle, l3) {
            continue;
        }

        let l3_first_l2 = l3 * 64;
        let l3_last_l2 = l3_first_l2 + 64;
        let first_l2 = ((start / SAMPLES_PER_L2_BIT) as usize).max(l3_first_l2);
        let last_l2 = ((end - 1) / SAMPLES_PER_L2_BIT) as usize;
        let l2_end = (last_l2 + 1).min(l3_last_l2);
        let mut l2_bits = *levels.l2_toggle.get(l3)?;
        l2_bits &= range_mask(first_l2 - l3_first_l2, l2_end - l3_first_l2);

        while let Some(l2_offset) = nonzero_trailing_bit(l2_bits) {
            l2_bits &= l2_bits - 1;
            let l2 = l3_first_l2 + l2_offset;
            let l2_first_l1 = l2 * 64;
            let l2_last_l1 = l2_first_l1 + 64;
            let first_l1 = ((start / SAMPLES_PER_L1_BIT) as usize).max(l2_first_l1);
            let last_l1 = ((end - 1) / SAMPLES_PER_L1_BIT) as usize;
            let l1_end = (last_l1 + 1).min(l2_last_l1);
            let mut l1_bits = *levels.l1_toggle.get(l2)?;
            l1_bits &= range_mask(first_l1 - l2_first_l1, l1_end - l2_first_l1);
            if let Some(l1_offset) = nonzero_trailing_bit(l1_bits) {
                return Some(l2_first_l1 + l1_offset);
            }
        }
    }
    None
}

fn nonzero_trailing_bit(word: u64) -> Option<usize> {
    (word != 0).then(|| word.trailing_zeros() as usize)
}

#[allow(clippy::too_many_arguments)]
fn append_raw_transitions(
    data: &[u8],
    block_start: u64,
    local_start: u64,
    local_end: u64,
    previous_block_last: Option<bool>,
    max_transitions: usize,
    output: &mut Vec<CaptureTransition>,
) {
    if local_start >= local_end || output.len() >= max_transitions {
        return;
    }

    let word_index = local_start as usize / 64;
    let word_start = word_index * 64;
    let word = load_le_word(data, word_index);
    let entering = if word_start > 0 {
        packed_bit(data, word_start - 1)
    } else {
        previous_block_last.unwrap_or(word & 1 != 0)
    };
    let lo = local_start as usize - word_start;
    let hi = local_end as usize - word_start;
    let mut toggles = word ^ ((word << 1) | entering as u64);
    toggles &= range_mask(lo, hi);

    while output.len() < max_transitions {
        let Some(bit_index) = nonzero_trailing_bit(toggles) else {
            break;
        };
        toggles &= toggles - 1;
        output.push(CaptureTransition {
            sample: block_start + (word_start + bit_index) as u64,
            value: bit(word, bit_index),
        });
    }
}

impl<R: BlockCaptureSource> CaptureIndex for IndexSampler<R> {
    fn display_name(&self) -> String {
        self.display_name()
    }
    fn index_path(&self) -> &Path {
        self.index_path()
    }
    fn header(&self) -> &CaptureMetadata {
        self.header()
    }
    fn capture_duration_us(&self) -> f64 {
        self.capture_duration_us()
    }
    fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> Result<CaptureSampledWindow> {
        self.sampled_window(channels, start_sample, end_sample, target_points)
    }
}
