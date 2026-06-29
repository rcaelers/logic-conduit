use super::storage::IndexStorage;
use super::types::{SAMPLES_PER_L1_BIT, SAMPLES_PER_L2_BIT, SAMPLES_PER_L3_BIT, bit};

#[derive(Clone, Copy)]
struct GroupSummary {
    first: bool,
    toggle: bool,
    last: bool,
}
use crate::runtime::{
    BlockCaptureSource, CaptureActivity, CaptureBucket, CaptureMetadata, CaptureSampledChannel,
    CaptureSampledWindow, CaptureTransition, packed_bit,
};
use crate::{Error, Result};
use std::path::Path;

const EXACT_SCAN_MAX_SAMPLES: u64 = 2_000_000;

/// Retrieval API used by the logic analyzer viewer.
///
/// This type assumes index construction and sidecar loading are already handled.
/// It only samples visible windows from an [`IndexStorage`] and falls back to a
/// raw reader for deep zoom levels.
pub(super) struct IndexReader<R: BlockCaptureSource> {
    storage: IndexStorage,
    raw_reader: R,
}

impl<R> IndexReader<R>
where
    R: BlockCaptureSource,
{
    pub(super) fn new(storage: IndexStorage, raw_reader: R) -> Self {
        Self { storage, raw_reader }
    }

    pub(super) fn with_max_cached_leaves(mut self, max: usize) -> Self {
        self.storage.set_max_cached_leaves(max);
        self
    }

    pub(super) fn index_path(&self) -> &Path {
        self.storage.path()
    }

    pub(super) fn header(&self) -> &CaptureMetadata {
        self.storage.header()
    }

    pub(super) fn capture_duration_us(&self) -> f64 {
        self.header().total_samples as f64 * 1_000_000.0 / self.header().samplerate_hz
    }

    pub(super) fn sampled_window(
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
        let target_points = target_points.max(1) as u64;
        let sample_step = samples.div_ceil(target_points).max(1);

        if samples <= EXACT_SCAN_MAX_SAMPLES {
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
            let block_end = block_start.saturating_add(samples_per_block).min(end_sample);
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
            activities: Vec::new(),
            buckets: Vec::new(),
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
        let mut activities = Vec::new();
        let mut buckets = Vec::new();

        let samples = end_sample - start_sample;
        let target_points = target_points.max(1) as u64;
        let mut previous_end = start_sample;

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

            let summary = self.range_summary(channel, visible_start, visible_end, group_samples)?;
            buckets.push(CaptureBucket {
                start_sample: visible_start,
                end_sample: visible_end,
                first: summary.first,
                toggle: summary.toggle,
                last: summary.last,
            });
            if summary.toggle {
                activities.push(CaptureActivity {
                    start_sample: visible_start,
                    end_sample: visible_end,
                });
            }
        }

        Ok(CaptureSampledChannel {
            channel,
            name,
            initial,
            transitions,
            activities,
            buckets,
        })
    }

    fn range_summary(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
        group_samples: u64,
    ) -> Result<GroupSummary> {
        if group_samples >= self.header().samples_per_block {
            return self.block_level_range_summary(channel, start_sample, end_sample);
        }

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

        Ok(GroupSummary { first, toggle, last })
    }

    fn block_level_range_summary(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
    ) -> Result<GroupSummary> {
        let first_block = self.block_for_sample(start_sample);
        let last_block = self.block_for_sample(end_sample.saturating_sub(1));
        let first_entry = self.storage.load_root_summary(channel, first_block as usize)?;
        let first = first_entry.first;
        let mut last = first_entry.last;
        let mut toggle = first_entry.toggle;

        for block in first_block.saturating_add(1)..=last_block {
            let entry = self.storage.load_root_summary(channel, block as usize)?;
            toggle |= entry.first != last || entry.toggle;
            last = entry.last;
        }

        Ok(GroupSummary { first, toggle, last })
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
        Ok(GroupSummary { first, toggle, last })
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
                first: if idx == 0 { entry.first } else { bit(entry.l3_last, idx - 1) },
                toggle: bit(entry.l3_toggle, idx),
                last: bit(entry.l3_last, idx),
            });
        }

        let leaf = self.storage.load_leaf(channel, block as usize)?;

        let Some(levels) = leaf.levels.as_ref() else {
            return Ok(GroupSummary { first: leaf.first, toggle: false, last: leaf.first });
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
