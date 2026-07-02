use super::builder::IndexBuilder;
use super::exact_window_sample_limit;
use super::storage::IndexReader;
use super::types::{
    CaptureIndexProgress, SAMPLES_PER_L1_BIT, SAMPLES_PER_L2_BIT, SAMPLES_PER_L3_BIT, bit,
};
use crate::runtime::{
    BlockCaptureSource, CaptureDataSource, CaptureMetadata, CaptureSampledChannel,
    CaptureSampledWindow, CaptureTransition, CaptureWaveformSegment, packed_bit,
};
use crate::{Error, Result};
use std::path::Path;

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
}

impl<R> IndexSampler<R>
where
    R: BlockCaptureSource,
{
    pub(super) fn new(display_name: String, storage: IndexReader, raw_reader: R) -> Self {
        Self {
            display_name,
            storage,
            raw_reader,
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

        let storage = IndexReader::open(index_path, header, fingerprint.revision)?;
        let display_name = data_source.display_name();
        let raw_reader = data_source.open_reader()?;
        Ok(Self::new(display_name, storage, raw_reader))
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
                target_points as usize,
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
        let mut current = self.raw_reader.read_sample(channel, start_sample)?;
        let initial = current;
        let mut transitions = Vec::new();

        let first_block = start_sample / samples_per_block;
        let last_block = (end_sample - 1) / samples_per_block;
        for block in first_block..=last_block {
            let data = self.raw_reader.read_packed_block(channel, block)?;
            let block_start = block * samples_per_block;
            let block_end = block_start
                .saturating_add(samples_per_block)
                .min(end_sample);
            let sample_start = start_sample.max(block_start);

            for sample in sample_start..block_end {
                if sample == start_sample {
                    continue;
                }

                let local_sample = (sample - block_start) as usize;
                let value = packed_bit(&data, local_sample);
                if value != current {
                    current = value;
                    transitions.push(CaptureTransition { sample, value });
                }
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
            waveform.push(CaptureWaveformSegment::Activity {
                start_sample,
                end_sample,
                first: *previous_value,
                last: summary.last,
            });
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
