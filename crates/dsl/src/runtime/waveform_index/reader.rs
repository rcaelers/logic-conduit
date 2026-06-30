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

    pub fn with_max_cached_leaves(mut self, max: usize) -> Self {
        self.storage.set_max_cached_leaves(max);
        self
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
        let initial = self.raw_reader.read_sample(channel, start_sample)?;
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

            let summary = self.display_range_summary(
                channel,
                visible_start,
                visible_end,
                group_samples,
                previous_value,
            )?;
            self.append_pixel_waveform(
                channel,
                visible_start,
                visible_end,
                group_samples,
                summary,
                &mut previous_value,
                &mut waveform,
            )?;
        }

        Ok(CaptureSampledChannel {
            channel,
            name,
            initial,
            transitions,
            waveform,
        })
    }

    fn display_range_summary(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
        group_samples: u64,
        first: bool,
    ) -> Result<GroupSummary> {
        self.indexed_display_range_summary(channel, start_sample, end_sample, group_samples, first)
    }

    fn indexed_display_range_summary(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
        group_samples: u64,
        first: bool,
    ) -> Result<GroupSummary> {
        let mut toggle = false;
        let mut last = first;
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
            toggle |= summary.toggle || summary.first != last;
            last = summary.last;
        }

        toggle |= first != last;
        Ok(GroupSummary {
            first,
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
        if group_samples >= self.header().samples_per_block {
            let entry = self.storage.load_root_summary(channel, block)?;
            return Ok(GroupSummary {
                first: entry.first,
                toggle: entry.toggle,
                last: entry.last,
            });
        }

        if group_samples == SAMPLES_PER_L3_BIT {
            let entry = self.storage.load_root_summary(channel, block)?;
            let first_group = (local_start / SAMPLES_PER_L3_BIT).min(63) as usize;
            let last_group = ((local_end - 1) / SAMPLES_PER_L3_BIT).min(63) as usize;
            return Ok(GroupSummary {
                first: if first_group == 0 {
                    entry.first
                } else {
                    bit(entry.l3_last, first_group - 1)
                },
                toggle: bit_range_any(&[entry.l3_toggle], first_group, last_group),
                last: bit(entry.l3_last, last_group),
            });
        }

        let leaf = self.storage.load_leaf(channel, block)?;
        let Some(levels) = leaf.levels.as_ref() else {
            return Ok(GroupSummary {
                first: leaf.first,
                toggle: false,
                last: leaf.first,
            });
        };

        if group_samples == SAMPLES_PER_L2_BIT {
            let first_group = (local_start / SAMPLES_PER_L2_BIT).min(4095) as usize;
            let last_group = ((local_end - 1) / SAMPLES_PER_L2_BIT).min(4095) as usize;
            Ok(GroupSummary {
                first: if first_group == 0 {
                    leaf.first
                } else {
                    bit(
                        levels.l2_last[(first_group - 1) / 64],
                        (first_group - 1) % 64,
                    )
                },
                toggle: bit_range_any(&levels.l2_toggle, first_group, last_group),
                last: bit(levels.l2_last[last_group / 64], last_group % 64),
            })
        } else {
            let first_group = (local_start / SAMPLES_PER_L1_BIT).min(262_143) as usize;
            let last_group = ((local_end - 1) / SAMPLES_PER_L1_BIT).min(262_143) as usize;
            Ok(GroupSummary {
                first: if first_group == 0 {
                    leaf.first
                } else {
                    bit(
                        levels.l1_last[(first_group - 1) / 64],
                        (first_group - 1) % 64,
                    )
                },
                toggle: bit_range_any(&levels.l1_toggle, first_group, last_group),
                last: bit(levels.l1_last[last_group / 64], last_group % 64),
            })
        }
    }

    fn append_pixel_waveform(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
        group_samples: u64,
        summary: GroupSummary,
        previous_value: &mut bool,
        waveform: &mut Vec<CaptureWaveformSegment>,
    ) -> Result<()> {
        if end_sample <= start_sample {
            return Ok(());
        }

        let has_entry_edge = summary.first != *previous_value;
        let has_internal_toggle = if has_entry_edge && summary.toggle {
            if end_sample > start_sample + 1 {
                let interior =
                    self.range_summary(channel, start_sample + 1, end_sample, group_samples)?;
                interior.toggle || interior.first != summary.first
            } else {
                false
            }
        } else {
            summary.toggle
        };

        if !has_entry_edge && !has_internal_toggle {
            push_level(waveform, start_sample, end_sample, summary.first);
            *previous_value = summary.last;
            return Ok(());
        }

        if has_entry_edge {
            waveform.push(CaptureWaveformSegment::Edge {
                sample: start_sample,
                before: *previous_value,
                after: summary.first,
            });
            *previous_value = summary.first;
        }

        if has_internal_toggle {
            waveform.push(CaptureWaveformSegment::Activity {
                start_sample,
                end_sample,
                first: *previous_value,
                last: summary.last,
            });
        } else {
            push_level(waveform, start_sample, end_sample, *previous_value);
        }
        *previous_value = summary.last;
        Ok(())
    }

    fn range_summary(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
        group_samples: u64,
    ) -> Result<GroupSummary> {
        let first = self.raw_reader.read_sample(channel, start_sample)?;
        let mut toggle = false;
        let mut last = first;
        let mut pos = start_sample;

        while pos < end_sample {
            let (chunk_start, chunk_samples) =
                self.next_summary_chunk(pos, end_sample, group_samples);
            let summary = if chunk_samples < SAMPLES_PER_L1_BIT {
                self.l0_range_summary(channel, chunk_start, chunk_start + chunk_samples)?
            } else {
                self.group_summary(channel, chunk_start, chunk_samples)?
            };
            toggle |= summary.toggle || summary.first != last;
            last = summary.last;
            pos = chunk_start + chunk_samples;
            if chunk_samples == 0 {
                break;
            }
        }

        Ok(GroupSummary {
            first,
            toggle,
            last,
        })
    }

    fn next_summary_chunk(
        &self,
        sample: u64,
        end_sample: u64,
        preferred_group_samples: u64,
    ) -> (u64, u64) {
        let remaining = end_sample - sample;
        let samples_per_block = self.header().samples_per_block;

        for group_samples in [
            samples_per_block,
            SAMPLES_PER_L3_BIT,
            SAMPLES_PER_L2_BIT,
            SAMPLES_PER_L1_BIT,
        ] {
            if group_samples > preferred_group_samples {
                continue;
            }
            if sample.is_multiple_of(group_samples) && remaining >= group_samples {
                return (sample, group_samples);
            }
        }

        let next_l1 = ((sample / SAMPLES_PER_L1_BIT) + 1) * SAMPLES_PER_L1_BIT;
        let chunk_end = next_l1.min(end_sample);
        (sample, chunk_end - sample)
    }

    fn l0_range_summary(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
    ) -> Result<GroupSummary> {
        let first = self.raw_reader.read_sample(channel, start_sample)?;
        let mut last = first;
        let mut toggle = false;
        for sample in start_sample.saturating_add(1)..end_sample {
            let value = self.raw_reader.read_sample(channel, sample)?;
            toggle |= value != last;
            last = value;
        }
        Ok(GroupSummary {
            first,
            toggle,
            last,
        })
    }

    fn group_summary(
        &mut self,
        channel: usize,
        sample: u64,
        group_samples: u64,
    ) -> Result<GroupSummary> {
        let sample = sample.min(self.header().total_samples.saturating_sub(1));
        let block = self.block_for_sample(sample);
        let local = sample - block * self.header().samples_per_block;

        if group_samples >= self.header().samples_per_block {
            let entry = self.storage.load_root_summary(channel, block as usize)?;
            return Ok(GroupSummary {
                first: entry.first,
                toggle: entry.toggle,
                last: entry.last,
            });
        }

        if group_samples == SAMPLES_PER_L3_BIT {
            let entry = self.storage.load_root_summary(channel, block as usize)?;
            let idx = (local / SAMPLES_PER_L3_BIT).min(63) as usize;
            return Ok(GroupSummary {
                first: if idx == 0 {
                    entry.first
                } else {
                    bit(entry.l3_last, idx - 1)
                },
                toggle: bit(entry.l3_toggle, idx),
                last: bit(entry.l3_last, idx),
            });
        }

        let leaf = self.storage.load_leaf(channel, block as usize)?;

        let Some(levels) = leaf.levels.as_ref() else {
            return Ok(GroupSummary {
                first: leaf.first,
                toggle: false,
                last: leaf.first,
            });
        };

        Ok(match group_samples {
            SAMPLES_PER_L2_BIT => {
                let group = (local / SAMPLES_PER_L2_BIT).min(4095) as usize;
                GroupSummary {
                    first: if group == 0 {
                        leaf.first
                    } else {
                        bit(levels.l2_last[(group - 1) / 64], (group - 1) % 64)
                    },
                    toggle: bit(levels.l2_toggle[group / 64], group % 64),
                    last: bit(levels.l2_last[group / 64], group % 64),
                }
            }
            _ => {
                let group = (local / SAMPLES_PER_L1_BIT).min(262_143) as usize;
                GroupSummary {
                    first: if group == 0 {
                        leaf.first
                    } else {
                        bit(levels.l1_last[(group - 1) / 64], (group - 1) % 64)
                    },
                    toggle: bit(levels.l1_toggle[group / 64], group % 64),
                    last: bit(levels.l1_last[group / 64], group % 64),
                }
            }
        })
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
