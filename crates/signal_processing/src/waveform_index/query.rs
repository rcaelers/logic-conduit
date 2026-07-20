use crate::{CaptureSampledChannel, CaptureWaveformSegment, Result};

#[derive(Clone, Copy, Debug)]
pub(crate) struct GroupSummary {
    pub first: bool,
    pub toggle: bool,
    pub last: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SummaryGrid {
    pub(crate) start_sample: u64,
    pub(crate) available_end_sample: u64,
    pub(crate) grid_end_sample: u64,
    pub(crate) target_points: usize,
}

pub(crate) fn sample_summary_channel(
    channel: usize,
    name: String,
    initial: bool,
    grid: SummaryGrid,
    mut summarize: impl FnMut(u64, u64, bool) -> Result<GroupSummary>,
) -> Result<CaptureSampledChannel> {
    let mut waveform = Vec::new();
    let grid_samples = grid.grid_end_sample.saturating_sub(grid.start_sample);
    let target_points = grid.target_points.max(1) as u64;
    let mut previous_end = grid.start_sample;
    let mut previous_value = initial;

    for point in 0..target_points {
        let visible_start =
            grid.start_sample + grid_samples.saturating_mul(point) / target_points;
        if visible_start >= grid.available_end_sample {
            break;
        }
        let visible_end = if point + 1 == target_points {
            grid.grid_end_sample
        } else {
            grid.start_sample + grid_samples.saturating_mul(point + 1) / target_points
        }
        .min(grid.available_end_sample);
        if visible_end <= visible_start || visible_start < previous_end {
            continue;
        }
        previous_end = visible_end;

        let summary = summarize(visible_start, visible_end, previous_value)?;
        append_pixel_waveform(
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
        transitions: Vec::new(),
        waveform,
    })
}

fn append_pixel_waveform(
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

#[cfg(test)]
mod tests {
    use super::{GroupSummary, SummaryGrid, sample_summary_channel};

    #[test]
    fn growing_prefix_uses_the_planned_viewport_grid() {
        let mut early_ranges = Vec::new();
        sample_summary_channel(
            0,
            "clk".into(),
            false,
            SummaryGrid {
                start_sample: 0,
                available_end_sample: 35,
                grid_end_sample: 100,
                target_points: 10,
            },
            |start, end, previous| {
                early_ranges.push((start, end));
                Ok(GroupSummary {
                    first: previous,
                    toggle: false,
                    last: previous,
                })
            },
        )
        .unwrap();

        let mut complete_ranges = Vec::new();
        sample_summary_channel(
            0,
            "clk".into(),
            false,
            SummaryGrid {
                start_sample: 0,
                available_end_sample: 100,
                grid_end_sample: 100,
                target_points: 10,
            },
            |start, end, previous| {
                complete_ranges.push((start, end));
                Ok(GroupSummary {
                    first: previous,
                    toggle: false,
                    last: previous,
                })
            },
        )
        .unwrap();

        assert_eq!(early_ranges, vec![(0, 10), (10, 20), (20, 30), (30, 35)]);
        assert_eq!(&complete_ranges[..3], &early_ranges[..3]);
    }
}
