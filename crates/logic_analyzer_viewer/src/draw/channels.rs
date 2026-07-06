use crate::channel::LogicChannel;
use crate::format::badge_text_color;
use crate::types::WaveformSegmentKind;
use crate::viewer::LogicAnalyzerViewer;
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, vec2};

impl LogicAnalyzerViewer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_channels(
        &self,
        painter: &Painter,
        labels_rect: Rect,
        wave_rect: Rect,
        row_height: f32,
        name_col_width: f32,
        badge_width: f32,
        text: Color32,
        trace: Color32,
        grid: Color32,
    ) {
        let clip = painter.with_clip_rect(wave_rect);

        for (index, channel) in self.channels.iter().enumerate() {
            let y_top = labels_rect.top() + index as f32 * row_height;
            let row_rect = Rect::from_min_max(
                Pos2::new(labels_rect.left(), y_top),
                Pos2::new(wave_rect.right(), y_top + row_height),
            );
            if row_rect.top() > labels_rect.bottom() {
                break;
            }

            painter.line_segment(
                [
                    Pos2::new(labels_rect.left(), row_rect.bottom()),
                    Pos2::new(wave_rect.right(), row_rect.bottom()),
                ],
                Stroke::new(1.0, Color32::from_rgb(42, 42, 42)),
            );

            let name_pos = Pos2::new(labels_rect.left() + 12.0, row_rect.center().y);
            painter.text(
                name_pos,
                Align2::LEFT_CENTER,
                &channel.name,
                FontId::proportional(12.0),
                text,
            );

            let badge_rect = Rect::from_min_size(
                Pos2::new(
                    labels_rect.left() + 12.0 + name_col_width + 10.0,
                    row_rect.center().y - 8.0,
                ),
                vec2(badge_width, 16.0),
            );
            let badge_color = self.color_profile.channel_color(channel.index);
            painter.rect_filled(badge_rect, 2.0, badge_color);
            painter.text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                channel.index.to_string(),
                FontId::monospace(10.0),
                badge_text_color(badge_color),
            );

            let center_y = row_rect.center().y;
            clip.line_segment(
                [
                    Pos2::new(wave_rect.left(), center_y),
                    Pos2::new(wave_rect.right(), center_y),
                ],
                Stroke::new(1.0, grid),
            );
            self.draw_channel_waveform(&clip, wave_rect, y_top, row_height, channel, trace);
        }
    }

    fn draw_channel_waveform(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        y_top: f32,
        row_height: f32,
        channel: &LogicChannel,
        trace: Color32,
    ) {
        let high_y = y_top + row_height * 0.28;
        let low_y = y_top + row_height * 0.72;
        let start = self.visible_start_us;
        let end = start + self.visible_span_us;
        let stroke = Stroke::new(1.4, trace);

        if !channel.waveform.is_empty() {
            self.draw_segment_waveform(painter, wave_rect, high_y, low_y, channel, trace);
            return;
        }

        let (visible_transitions, mut value) = channel.visible_transitions(start, end);
        let mut prev_x = wave_rect.left();
        let mut y = if value { high_y } else { low_y };

        for transition in visible_transitions {
            let x = self.time_to_x(wave_rect, transition.time_us);
            painter.line_segment([Pos2::new(prev_x, y), Pos2::new(x, y)], stroke);

            value = transition.value;
            let next_y = if value { high_y } else { low_y };
            painter.line_segment([Pos2::new(x, y), Pos2::new(x, next_y)], stroke);

            prev_x = x;
            y = next_y;
        }

        painter.line_segment(
            [Pos2::new(prev_x, y), Pos2::new(wave_rect.right(), y)],
            stroke,
        );
    }

    fn draw_segment_waveform(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        high_y: f32,
        low_y: f32,
        channel: &LogicChannel,
        trace: Color32,
    ) {
        let start = self.visible_start_us;
        let end = start + self.visible_span_us;
        let flat_stroke = Stroke::new(1.15, trace);
        let activity_stroke = Stroke::new(1.0, trace);

        for segment in channel
            .waveform
            .iter()
            .filter(|segment| segment.end_us >= start && segment.start_us <= end)
        {
            let x0 = self.time_to_x(wave_rect, segment.start_us);
            let x1 = self.time_to_x(wave_rect, segment.end_us);

            match segment.kind {
                WaveformSegmentKind::Level { value } => {
                    let y = if value { high_y } else { low_y };
                    Self::draw_clipped_horizontal(painter, wave_rect, x0, x1, y, flat_stroke);
                }
                WaveformSegmentKind::Edge { before, after } => {
                    let y0 = if before { high_y } else { low_y };
                    let y1 = if after { high_y } else { low_y };
                    painter.line_segment([Pos2::new(x0, y0), Pos2::new(x0, y1)], activity_stroke);
                }
                WaveformSegmentKind::Activity { first, last } => {
                    Self::draw_activity_summary(
                        painter,
                        wave_rect,
                        x0,
                        x1,
                        high_y,
                        low_y,
                        first,
                        last,
                        flat_stroke,
                        activity_stroke,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_activity_summary(
        painter: &Painter,
        clip: Rect,
        x0: f32,
        x1: f32,
        high_y: f32,
        low_y: f32,
        first: bool,
        last: bool,
        flat_stroke: Stroke,
        activity_stroke: Stroke,
    ) {
        let left = x0.min(x1).max(clip.left());
        let right = x0.max(x1).min(clip.right());
        if right <= left {
            return;
        }

        // An activity segment wider than a couple of pixels (a coarse window
        // stretched by zooming in) only promises "at least one toggle in this
        // range" — draw it as a solid band rather than inventing edge
        // positions that a refresh would then contradict.
        if right - left > 3.0 {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(left, high_y), Pos2::new(right, low_y)),
                0.0,
                flat_stroke.color,
            );
            return;
        }

        let y_first = if first { high_y } else { low_y };
        let y_last = if last { high_y } else { low_y };
        let marker_x = ((left + right) * 0.5).clamp(clip.left(), clip.right());

        if first == last {
            Self::draw_clipped_horizontal(painter, clip, left, right, y_last, flat_stroke);
        } else if right - left >= 4.0 {
            Self::draw_clipped_horizontal(painter, clip, left, marker_x, y_first, flat_stroke);
            Self::draw_clipped_horizontal(painter, clip, marker_x, right, y_last, flat_stroke);
        } else {
            Self::draw_clipped_horizontal(painter, clip, left, right, y_last, flat_stroke);
        }

        painter.line_segment(
            [Pos2::new(marker_x, high_y), Pos2::new(marker_x, low_y)],
            activity_stroke,
        );
    }

    fn draw_clipped_horizontal(
        painter: &Painter,
        clip: Rect,
        x0: f32,
        x1: f32,
        y: f32,
        stroke: Stroke,
    ) {
        let left = x0.min(x1).max(clip.left());
        let right = x0.max(x1).min(clip.right());
        if right > left {
            painter.line_segment([Pos2::new(left, y), Pos2::new(right, y)], stroke);
        }
    }
}
