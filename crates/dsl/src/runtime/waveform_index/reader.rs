use super::storage::IndexStorage;
use super::types::{
    BLOCKS_PER_ROOT, GroupSummary, L1_GROUP_SAMPLES, L2_GROUP_SAMPLES, L3_GROUP_SAMPLES, bit,
};
use crate::runtime::{
    CaptureActivity, CaptureBucket, CaptureMetadata, CaptureSampledChannel, CaptureSampledWindow,
    CaptureSource, CaptureTransition,
};
use crate::{Error, Result};
use std::path::Path;

/// Retrieval API used by the logic analyzer viewer.
///
/// This type assumes index construction and sidecar loading are already handled.
/// It only samples visible windows from an [`IndexStorage`] and falls back to a
/// raw reader for deep zoom levels.
pub(super) struct IndexReader<R: CaptureSource> {
    storage: IndexStorage,
    raw_reader: R,
}

impl<R> IndexReader<R>
where
    R: CaptureSource,
{
    pub(super) fn new(storage: IndexStorage, raw_reader: R) -> Self {
        Self {
            storage,
            raw_reader,
        }
    }

    pub(super) fn with_max_cached_roots(mut self, max_cached_roots: usize) -> Self {
        self.storage.set_max_cached_roots(max_cached_roots);
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

        if sample_step < L1_GROUP_SAMPLES {
            return self.raw_reader.sampled_window(
                channels,
                start_sample,
                end_sample,
                target_points as usize,
            );
        }

        let group_samples = if sample_step >= self.header().samples_per_block {
            self.header().samples_per_block
        } else if sample_step >= L3_GROUP_SAMPLES {
            L3_GROUP_SAMPLES
        } else if sample_step >= L2_GROUP_SAMPLES {
            L2_GROUP_SAMPLES
        } else {
            L1_GROUP_SAMPLES
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
        let initial = self.value_at_group_start(channel, start_sample, group_samples)?;
        let mut current = initial;
        let mut transitions = Vec::new();
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
                toggle: summary.toggle,
                last: summary.last,
            });
            if summary.toggle {
                activities.push(CaptureActivity {
                    start_sample: visible_start,
                    end_sample: visible_end,
                });
                let visible_edge = visible_start + (visible_end - visible_start) / 2;
                if summary.last != current {
                    transitions.push(CaptureTransition {
                        sample: visible_edge,
                        value: summary.last,
                    });
                }
            }
            current = summary.last;
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

    fn value_at_group_start(
        &mut self,
        channel: usize,
        sample: u64,
        group_samples: u64,
    ) -> Result<bool> {
        if sample == 0 {
            let block = self.block_for_sample(0);
            let root_index = (block as usize) / BLOCKS_PER_ROOT;
            let root = self.storage.load_root(channel, root_index)?;
            let leaf_index = (block - root.first_block) as usize;
            return Ok(root.leaves.get(leaf_index).is_some_and(|leaf| leaf.first));
        }

        let aligned = (sample / group_samples) * group_samples;
        if aligned == 0 {
            let block = self.block_for_sample(0);
            let root_index = (block as usize) / BLOCKS_PER_ROOT;
            let root = self.storage.load_root(channel, root_index)?;
            let leaf_index = (block - root.first_block) as usize;
            return Ok(root.leaves.get(leaf_index).is_some_and(|leaf| leaf.first));
        }
        self.group_summary(channel, aligned - group_samples, group_samples)
            .map(|g| g.last)
    }

    fn range_summary(
        &mut self,
        channel: usize,
        start_sample: u64,
        end_sample: u64,
        group_samples: u64,
    ) -> Result<GroupSummary> {
        let mut group_start = (start_sample / group_samples) * group_samples;
        let mut toggle = false;
        let mut last = self.value_at_group_start(channel, start_sample, group_samples)?;

        while group_start < end_sample {
            let group_end = group_start
                .saturating_add(group_samples)
                .min(self.header().total_samples);
            if group_end > start_sample {
                let summary = self.group_summary(channel, group_start, group_samples)?;
                toggle |= summary.toggle;
                last = summary.last;
            }
            if group_end <= group_start {
                break;
            }
            group_start = group_end;
        }

        Ok(GroupSummary { toggle, last })
    }

    fn group_summary(
        &mut self,
        channel: usize,
        sample: u64,
        group_samples: u64,
    ) -> Result<GroupSummary> {
        let sample = sample.min(self.header().total_samples.saturating_sub(1));
        let block_index = self.block_for_sample(sample);
        let local = sample - block_index * self.header().samples_per_block;
        let root_index = (block_index as usize) / BLOCKS_PER_ROOT;
        let root = self.storage.load_root(channel, root_index)?;
        let leaf_index = (block_index - root.first_block) as usize;
        let Some(leaf) = root.leaves.get(leaf_index) else {
            return Ok(GroupSummary {
                toggle: false,
                last: false,
            });
        };

        if !leaf.active {
            return Ok(GroupSummary {
                toggle: false,
                last: leaf.first,
            });
        }

        let summary = if group_samples >= self.header().samples_per_block {
            GroupSummary {
                toggle: leaf.active,
                last: leaf.last,
            }
        } else {
            match group_samples {
                L3_GROUP_SAMPLES => {
                    let idx = (local / L3_GROUP_SAMPLES).min(63) as usize;
                    GroupSummary {
                        toggle: bit(leaf.l3_toggle, idx),
                        last: bit(leaf.l3_last, idx),
                    }
                }
                L2_GROUP_SAMPLES => {
                    let group = (local / L2_GROUP_SAMPLES).min(4095) as usize;
                    let word = group / 64;
                    let bit_idx = group % 64;
                    GroupSummary {
                        toggle: bit(leaf.l2_toggle[word], bit_idx),
                        last: bit(leaf.l2_last[word], bit_idx),
                    }
                }
                _ => {
                    let group = (local / L1_GROUP_SAMPLES).min(262_143) as usize;
                    let word = group / 64;
                    let bit_idx = group % 64;
                    GroupSummary {
                        toggle: leaf
                            .l1_toggle
                            .get(word)
                            .is_some_and(|word| bit(*word, bit_idx)),
                        last: leaf
                            .l1_last
                            .get(word)
                            .is_some_and(|word| bit(*word, bit_idx)),
                    }
                }
            }
        };

        Ok(summary)
    }

    fn block_for_sample(&self, sample: u64) -> u64 {
        sample / self.header().samples_per_block
    }
}
