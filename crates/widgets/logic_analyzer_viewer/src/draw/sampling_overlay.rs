use egui::{Color32, Painter, Pos2, Shape, Stroke};

use crate::channel::LogicChannel;
use crate::sampling_overlay::SamplingEdge;
use crate::types::{AnalyzerLayout, RowKey, WaveformSegmentKind};
use crate::viewer::LogicAnalyzerViewer;

const MARKER_SPACING: f32 = 6.0;

impl LogicAnalyzerViewer {
    pub(super) fn draw_sampling_overlay(&self, painter: &Painter, layout: AnalyzerLayout) {
        let Some(overlay) = &self.sampling_overlay else {
            return;
        };
        let Some(clock) = self
            .channels
            .iter()
            .find(|channel| channel.index == overlay.clock_channel)
        else {
            return;
        };
        let Some(clock_row) = self
            .row_order
            .iter()
            .position(|row| row == &RowKey::Channel(overlay.clock_channel))
        else {
            return;
        };

        let edges = visible_sampling_edges(
            clock,
            overlay.edge,
            self.visible_start_us,
            self.visible_start_us + self.visible_span_us,
        );
        if edges.is_empty()
            || edges.len() as f32 > (layout.wave_rect.width() / MARKER_SPACING).max(1.0)
        {
            return;
        }

        let clip = painter.with_clip_rect(layout.wave_rect);
        let clock_top = self.row_top(layout.wave_rect.top(), clock_row, layout.row_height);
        let marker_color = Color32::from_rgb(0, 220, 95);
        for &(time_us, rising) in &edges {
            let x = self.time_to_x(layout.wave_rect, time_us);
            draw_clock_arrow(&clip, x, clock_top, layout.row_height, rising, marker_color);
        }

        for &channel_index in &overlay.sampled_channels {
            let Some(channel) = self
                .channels
                .iter()
                .find(|channel| channel.index == channel_index)
            else {
                continue;
            };
            let Some(row) = self
                .row_order
                .iter()
                .position(|key| key == &RowKey::Channel(channel_index))
            else {
                continue;
            };
            let row_top = self.row_top(layout.wave_rect.top(), row, layout.row_height);
            let high_y = row_top + layout.row_height * 0.28;
            let low_y = row_top + layout.row_height * 0.72;
            for &(time_us, _) in &edges {
                let Some(value) = channel_value_at(channel, time_us) else {
                    continue;
                };
                let center = Pos2::new(
                    self.time_to_x(layout.wave_rect, time_us),
                    if value { high_y } else { low_y },
                );
                clip.circle_filled(center, 3.4, marker_color);
                clip.circle_stroke(center, 3.4, Stroke::new(0.8, Color32::from_rgb(12, 40, 24)));
            }
        }
    }
}

fn visible_sampling_edges(
    channel: &LogicChannel,
    edge: SamplingEdge,
    start_us: f64,
    end_us: f64,
) -> Vec<(f64, bool)> {
    let mut edges: Vec<(f64, bool)> = if channel.waveform.is_empty() {
        channel
            .visible_transitions(start_us, end_us)
            .0
            .iter()
            .filter(|transition| edge.accepts(transition.value))
            .map(|transition| (transition.time_us, transition.value))
            .collect()
    } else {
        if channel.waveform.iter().any(|segment| {
            segment.end_us >= start_us
                && segment.start_us <= end_us
                && matches!(segment.kind, WaveformSegmentKind::Activity { .. })
        }) {
            return Vec::new();
        }
        channel
            .waveform
            .iter()
            .filter_map(|segment| match segment.kind {
                WaveformSegmentKind::Edge { after, .. }
                    if segment.start_us >= start_us
                        && segment.start_us <= end_us
                        && edge.accepts(after) =>
                {
                    Some((segment.start_us, after))
                }
                _ => None,
            })
            .collect()
    };
    edges.sort_by(|left, right| left.0.total_cmp(&right.0));
    edges.dedup_by(|left, right| left.0 == right.0);
    edges
}

fn channel_value_at(channel: &LogicChannel, time_us: f64) -> Option<bool> {
    if channel.waveform.is_empty() {
        let index = channel
            .transitions
            .partition_point(|transition| transition.time_us <= time_us);
        return Some(
            index
                .checked_sub(1)
                .and_then(|index| channel.transitions.get(index))
                .map_or(channel.initial, |transition| transition.value),
        );
    }

    if let Some(value) = channel
        .waveform
        .iter()
        .find_map(|segment| match segment.kind {
            WaveformSegmentKind::Edge { after, .. } if segment.start_us == time_us => Some(after),
            _ => None,
        })
    {
        return Some(value);
    }
    channel.waveform.iter().find_map(|segment| {
        if time_us < segment.start_us || time_us > segment.end_us {
            return None;
        }
        match segment.kind {
            WaveformSegmentKind::Level { value } => Some(value),
            WaveformSegmentKind::Edge { after, .. } => Some(after),
            WaveformSegmentKind::Activity { .. } => None,
        }
    })
}

fn draw_clock_arrow(
    painter: &Painter,
    x: f32,
    row_top: f32,
    row_height: f32,
    rising: bool,
    color: Color32,
) {
    let high_y = row_top + row_height * 0.28;
    let low_y = row_top + row_height * 0.72;
    let (tip_y, base_y, stem_end) = if rising {
        (high_y - 2.0, high_y + 4.5, high_y + 8.0)
    } else {
        (low_y + 2.0, low_y - 4.5, low_y - 8.0)
    };
    painter.line_segment(
        [Pos2::new(x, base_y), Pos2::new(x, stem_end)],
        Stroke::new(1.2, color),
    );
    painter.add(Shape::convex_polygon(
        vec![
            Pos2::new(x, tip_y),
            Pos2::new(x - 4.0, base_y),
            Pos2::new(x + 4.0, base_y),
        ],
        color,
        Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Transition;

    fn channel() -> LogicChannel {
        LogicChannel {
            index: 0,
            name: "clock".into(),
            initial: false,
            transitions: vec![
                Transition {
                    time_us: 1.0,
                    value: true,
                },
                Transition {
                    time_us: 2.0,
                    value: false,
                },
                Transition {
                    time_us: 3.0,
                    value: true,
                },
            ],
            waveform: Vec::new(),
        }
    }

    #[test]
    fn edge_filter_distinguishes_sdr_and_ddr() {
        assert_eq!(
            visible_sampling_edges(&channel(), SamplingEdge::Rising, 0.0, 4.0),
            vec![(1.0, true), (3.0, true)]
        );
        assert_eq!(
            visible_sampling_edges(&channel(), SamplingEdge::Falling, 0.0, 4.0),
            vec![(2.0, false)]
        );
        assert_eq!(
            visible_sampling_edges(&channel(), SamplingEdge::Both, 0.0, 4.0).len(),
            3
        );
    }

    #[test]
    fn samples_level_after_transition_at_the_same_time() {
        assert_eq!(channel_value_at(&channel(), 0.5), Some(false));
        assert_eq!(channel_value_at(&channel(), 1.0), Some(true));
        assert_eq!(channel_value_at(&channel(), 2.5), Some(false));
    }

    #[test]
    fn indexed_edge_level_wins_over_the_run_ending_at_that_edge() {
        let mut channel = channel();
        channel.transitions.clear();
        channel.waveform = vec![
            crate::types::WaveformSegment {
                start_us: 0.0,
                end_us: 1.0,
                kind: WaveformSegmentKind::Level { value: false },
            },
            crate::types::WaveformSegment {
                start_us: 1.0,
                end_us: 1.0,
                kind: WaveformSegmentKind::Edge {
                    before: false,
                    after: true,
                },
            },
        ];
        assert_eq!(channel_value_at(&channel, 1.0), Some(true));
    }
}
