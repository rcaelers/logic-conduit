use egui::{Align2, Color32, FontId, Pos2, Rect, Shape, Stroke};

use signal_processing::{Annotation, MAX_ANNOTATION_NS, MipmapRecord, Sample};

use crate::lanes::{AnnotationVisual, OpaqueLaneDrawContext, ViewerLaneTheme};

const MIN_ANNOTATION_WIDTH_PX: f32 = 8.0;

/// Draws an exact digital transition snapshot supplied by a lane adapter.
pub fn draw_digital_snapshot(
    context: &OpaqueLaneDrawContext<'_>,
    samples: &[Sample],
    initial: bool,
) {
    let color = context.theme.accent;
    let stroke = Stroke::new(1.4, color);
    let high_y = context.top + context.height * 0.28;
    let low_y = context.top + context.height * 0.72;
    let y_of = |value: bool| if value { high_y } else { low_y };
    let mut level = initial;
    let mut previous_x = context.wave_rect.left();
    for sample in samples {
        let x = context.time_to_x(sample.start_time_ns);
        context.painter.line_segment(
            [
                Pos2::new(previous_x, y_of(level)),
                Pos2::new(x, y_of(level)),
            ],
            stroke,
        );
        context.painter.line_segment(
            [Pos2::new(x, y_of(level)), Pos2::new(x, y_of(sample.value))],
            stroke,
        );
        level = sample.value;
        previous_x = x;
    }
    context.painter.line_segment(
        [
            Pos2::new(previous_x, y_of(level)),
            Pos2::new(context.wave_rect.right(), y_of(level)),
        ],
        stroke,
    );
}

/// Draws a bounded activity summary supplied by a dense digital lane query.
pub fn draw_digital_activity(
    context: &OpaqueLaneDrawContext<'_>,
    records: &[MipmapRecord],
    initial: bool,
) {
    let color = context.theme.accent;
    let stroke = Stroke::new(1.4, color);
    let high_y = context.top + context.height * 0.28;
    let low_y = context.top + context.height * 0.72;
    let y_of = |value: bool| if value { high_y } else { low_y };
    let mut level = initial;
    let mut previous_x = context.wave_rect.left();
    for record in records {
        let start_ns = record.start_ns.max(context.visible_start_ns);
        let end_ns = record.end_ns.min(context.visible_end_ns);
        if start_ns > end_ns {
            continue;
        }
        let x0 = context.time_to_x(start_ns).max(context.wave_rect.left());
        let x1 = context
            .time_to_x(end_ns)
            .max(x0 + 1.0)
            .min(context.wave_rect.right());
        context.painter.line_segment(
            [
                Pos2::new(previous_x, y_of(level)),
                Pos2::new(x0, y_of(level)),
            ],
            stroke,
        );
        match record.level_hint {
            Some((first, last)) if first == last => {
                if level != first {
                    context.painter.line_segment(
                        [Pos2::new(x0, y_of(level)), Pos2::new(x0, y_of(first))],
                        stroke,
                    );
                }
                context.painter.line_segment(
                    [Pos2::new(x0, y_of(first)), Pos2::new(x1, y_of(first))],
                    stroke,
                );
                level = last;
            }
            Some((_, last)) => {
                context.painter.rect_filled(
                    Rect::from_min_max(Pos2::new(x0, high_y), Pos2::new(x1, low_y)),
                    0.0,
                    color,
                );
                level = last;
            }
            None => {
                context.painter.rect_filled(
                    Rect::from_min_max(Pos2::new(x0, high_y), Pos2::new(x1, low_y)),
                    0.0,
                    color,
                );
            }
        }
        previous_x = x1;
    }
    context.painter.line_segment(
        [
            Pos2::new(previous_x, y_of(level)),
            Pos2::new(context.wave_rect.right(), y_of(level)),
        ],
        stroke,
    );
}

/// Draws exact trigger markers supplied by a lane adapter.
pub fn draw_trigger_snapshot(context: &OpaqueLaneDrawContext<'_>, markers: &[u64]) {
    let color = context.theme.accent;
    let top = context.top + context.height * 0.18;
    let bottom = context.top + context.height * 0.82;
    for timestamp_ns in markers {
        let x = context.time_to_x(*timestamp_ns);
        context.painter.line_segment(
            [Pos2::new(x, top), Pos2::new(x, bottom)],
            Stroke::new(1.4, color),
        );
        context.painter.add(Shape::convex_polygon(
            vec![
                Pos2::new(x - 4.0, top),
                Pos2::new(x + 4.0, top),
                Pos2::new(x, top + 6.0),
            ],
            color,
            Stroke::NONE,
        ));
    }
}

/// Draws bounded trigger activity supplied by a dense lane query.
pub fn draw_trigger_activity(context: &OpaqueLaneDrawContext<'_>, records: &[MipmapRecord]) {
    let color = context.theme.accent;
    let top = context.top + context.height * 0.18;
    let bottom = context.top + context.height * 0.82;
    for record in records {
        let start_ns = record.start_ns.max(context.visible_start_ns);
        let end_ns = record.end_ns.min(context.visible_end_ns);
        if start_ns > end_ns || record.count == 0 {
            continue;
        }
        let x0 = context.time_to_x(start_ns).max(context.wave_rect.left());
        let x1 = context
            .time_to_x(end_ns)
            .max(x0 + 1.0)
            .min(context.wave_rect.right());
        context.painter.rect_filled(
            Rect::from_min_max(Pos2::new(x0, top), Pos2::new(x1, bottom)),
            0.0,
            color,
        );
    }
}

/// Draws exact labeled value spans supplied by a lane presentation adapter.
pub fn draw_value_snapshot(
    context: &OpaqueLaneDrawContext<'_>,
    values: &[(u64, String)],
    color: Color32,
) {
    if values.is_empty() {
        return;
    }
    let box_top = context.top + context.height * 0.12;
    let box_bottom = context.top + context.height * 0.88;
    if values.len() > context.wave_rect.width().max(1.0) as usize * 2 {
        let x0 = context
            .time_to_x(values[0].0.max(context.visible_start_ns))
            .max(context.wave_rect.left());
        context.painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, box_top),
                Pos2::new(context.wave_rect.right(), box_bottom),
            ),
            0.0,
            color,
        );
        return;
    }
    for (index, (start_time_ns, value)) in values.iter().enumerate() {
        let segment_start = (*start_time_ns).max(context.visible_start_ns);
        let segment_end = values
            .get(index + 1)
            .map_or(context.visible_end_ns, |next| next.0)
            .min(context.visible_end_ns);
        if segment_end < segment_start {
            continue;
        }
        let x0 = context
            .time_to_x(segment_start)
            .max(context.wave_rect.left());
        let x1 = context
            .time_to_x(segment_end)
            .max(x0 + 2.0)
            .min(context.wave_rect.right());
        let rect = Rect::from_min_max(Pos2::new(x0, box_top), Pos2::new(x1, box_bottom));
        context
            .painter
            .rect_filled(rect, 2.0, color.linear_multiply(0.35));
        context
            .painter
            .rect_stroke(rect, 2.0, Stroke::new(1.2, color), egui::StrokeKind::Inside);
        if let Some(position) =
            annotation_label_position(rect, context.wave_rect, annotation_label_width(value))
        {
            context.painter.text(
                position,
                Align2::CENTER_CENTER,
                value,
                FontId::monospace(12.0),
                context.theme.foreground,
            );
        }
    }
}

/// Draws bounded dense value activity supplied by a lane query.
pub fn draw_value_activity(
    context: &OpaqueLaneDrawContext<'_>,
    records: &[MipmapRecord],
    color: Color32,
) {
    let box_top = context.top + context.height * 0.12;
    let box_bottom = context.top + context.height * 0.88;
    for record in records {
        let start_ns = record.start_ns.max(context.visible_start_ns);
        let end_ns = record.end_ns.min(context.visible_end_ns);
        if start_ns > end_ns || record.count == 0 {
            continue;
        }
        let x0 = context.time_to_x(start_ns).max(context.wave_rect.left());
        let x1 = context
            .time_to_x(end_ns)
            .max(x0 + 1.0)
            .min(context.wave_rect.right());
        context.painter.rect_filled(
            Rect::from_min_max(Pos2::new(x0, box_top), Pos2::new(x1, box_bottom)),
            0.0,
            color,
        );
    }
}

pub fn default_annotation_visual(
    value: u64,
    display_format: Option<&str>,
    theme: &ViewerLaneTheme,
) -> AnnotationVisual {
    AnnotationVisual {
        label: format_value(value, display_format),
        fill: theme.accent.gamma_multiply(0.35),
        border: Stroke::new(1.0, theme.accent),
    }
}

fn format_value(value: u64, format: Option<&str>) -> String {
    match format.unwrap_or("Hex") {
        "Binary" => format!("{value:b}"),
        "Octal" => format!("{value:o}"),
        "Decimal" => value.to_string(),
        "ASCII" => char::from_u32(value as u32)
            .filter(|character| !character.is_control())
            .map_or_else(|| ".".to_string(), |character| character.to_string()),
        "Hex + ASCII" => {
            let ascii = char::from_u32(value as u32)
                .filter(|character| !character.is_control())
                .unwrap_or('.');
            format!("{value:02X} '{ascii}'")
        }
        _ => format!("{value:02X}"),
    }
}

/// Draws a bounded, exact annotation snapshot supplied by a payload-owned
/// query. The callback keeps value formatting and protocol semantics owned by
/// the concrete presentation adapter.
pub fn draw_annotation_snapshot<F>(
    context: &OpaqueLaneDrawContext<'_>,
    annotations: &[Annotation],
    last_timestamp_ns: Option<u64>,
    mut visual_for: F,
) where
    F: FnMut(u64) -> AnnotationVisual,
{
    let box_top = context.top + context.height * 0.12;
    let box_bottom = context.top + context.height * 0.88;
    for (index, annotation) in annotations.iter().enumerate() {
        if annotation.end_ns < context.visible_start_ns {
            continue;
        }
        let is_last_ever =
            index == annotations.len() - 1 && last_timestamp_ns == Some(annotation.start_ns);
        let previous_duration_ns = (index > 0).then(|| {
            let previous = &annotations[index - 1];
            previous.end_ns.saturating_sub(previous.start_ns)
        });
        let effective_end = annotation_box_end(annotation, is_last_ever, previous_duration_ns);
        let x0 = context.time_to_x(annotation.start_ns);
        let natural_x1 = annotation_right_x(x0, context.time_to_x(effective_end));
        let visual = visual_for(annotation.value);
        let label_width = annotation_label_width(&visual.label);
        let rect = Rect::from_min_max(Pos2::new(x0, box_top), Pos2::new(natural_x1, box_bottom));
        let bevel = (rect.height() * 0.20)
            .min(rect.width() * 0.18)
            .clamp(1.0, 10.0);
        context.painter.add(Shape::convex_polygon(
            vec![
                Pos2::new(rect.left() + bevel, rect.top()),
                Pos2::new(rect.right() - bevel, rect.top()),
                Pos2::new(rect.right(), rect.center().y),
                Pos2::new(rect.right() - bevel, rect.bottom()),
                Pos2::new(rect.left() + bevel, rect.bottom()),
                Pos2::new(rect.left(), rect.center().y),
            ],
            visual.fill,
            visual.border,
        ));
        if let Some(label_position) =
            annotation_label_position(rect, context.wave_rect, label_width)
        {
            context.painter.text(
                label_position,
                Align2::CENTER_CENTER,
                visual.label,
                FontId::monospace(10.0),
                context.theme.foreground,
            );
        }
    }
}

/// Draws a bounded coarse presence snapshot supplied by a payload-owned
/// query.
pub fn draw_annotation_presence<I>(context: &OpaqueLaneDrawContext<'_>, buckets: I)
where
    I: IntoIterator<Item = (u64, u64, u64)>,
{
    let color = context.theme.accent;
    let top = context.top + context.height * 0.12;
    let bottom = context.top + context.height * 0.88;
    for (bucket_start_ns, bucket_end_ns, item_count) in buckets {
        let start_ns = bucket_start_ns.max(context.visible_start_ns);
        let end_ns = bucket_end_ns.min(context.visible_end_ns);
        if start_ns > end_ns || item_count == 0 {
            continue;
        }
        let x0 = context.time_to_x(start_ns);
        let x1 = context
            .time_to_x(end_ns)
            .max(x0 + 1.0)
            .min(context.wave_rect.right());
        context.painter.rect_filled(
            Rect::from_min_max(Pos2::new(x0, top), Pos2::new(x1, bottom)),
            0.0,
            color,
        );
    }
}

/// Effective right edge for one annotation box. The most recent word ever
/// decoded has no successor to patch its `end_ns` (see `append_word` in
/// `derived_data_collector.rs`) — `start_ns == end_ns` forever, not just until the next
/// word arrives — so it's rendered open-ended using the previous word's
/// width as a same-framing estimate (falling back to the burst cap when
/// there's no previous word to measure), rather than collapsing to an
/// unreadable sliver.
pub(crate) fn annotation_box_end(
    annotation: &Annotation,
    is_last_ever: bool,
    previous_duration_ns: Option<u64>,
) -> u64 {
    if is_last_ever && annotation.end_ns == annotation.start_ns {
        let duration = previous_duration_ns
            .unwrap_or(MAX_ANNOTATION_NS)
            .min(MAX_ANNOTATION_NS);
        annotation.start_ns + duration
    } else {
        annotation.end_ns.max(annotation.start_ns)
    }
}

fn annotation_label_width(label: &str) -> f32 {
    (label.chars().count() as f32 * 6.2 + 10.0).max(26.0)
}

fn annotation_right_x(start_x: f32, time_end_x: f32) -> f32 {
    time_end_x.max(start_x + MIN_ANNOTATION_WIDTH_PX)
}

fn annotation_label_position(rect: Rect, wave_rect: Rect, label_width: f32) -> Option<Pos2> {
    let visible_rect = rect.intersect(wave_rect);
    (visible_rect.width() >= label_width).then(|| visible_rect.center())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_annotation_visual_uses_the_supplied_theme() {
        let theme =
            ViewerLaneTheme::from_visuals(&egui::Visuals::light(), Color32::from_rgb(20, 80, 160));
        let visual = default_annotation_visual(0x2a, Some("Hex"), &theme);

        assert_eq!(visual.label, "2A");
        assert_eq!(visual.border.color, theme.accent);
        assert_eq!(visual.fill, theme.accent.gamma_multiply(0.35));
    }

    #[test]
    fn last_ever_open_annotation_matches_the_previous_words_width() {
        // "HELLO\n": every earlier byte was ~10 bit-times wide, so the
        // dangling '\n' should render about that wide too, not the full
        // 1ms burst cap.
        let annotation = Annotation {
            start_ns: 100_000,
            end_ns: 100_000,
            value: 0x0A,
        };
        assert_eq!(annotation_box_end(&annotation, true, Some(10_000)), 110_000);
    }

    #[test]
    fn last_ever_open_annotation_falls_back_to_the_burst_cap_with_no_history() {
        let annotation = Annotation {
            start_ns: 1_000,
            end_ns: 1_000,
            value: 0x4F,
        };
        assert_eq!(
            annotation_box_end(&annotation, true, None),
            1_000 + MAX_ANNOTATION_NS
        );
    }

    #[test]
    fn last_ever_open_annotation_never_exceeds_the_burst_cap() {
        let annotation = Annotation {
            start_ns: 1_000,
            end_ns: 1_000,
            value: 0x4F,
        };
        assert_eq!(
            annotation_box_end(&annotation, true, Some(MAX_ANNOTATION_NS * 10)),
            1_000 + MAX_ANNOTATION_NS
        );
    }

    #[test]
    fn closed_annotation_keeps_its_patched_end() {
        let annotation = Annotation {
            start_ns: 1_000,
            end_ns: 1_500,
            value: 0x4F,
        };
        assert_eq!(annotation_box_end(&annotation, true, Some(10_000)), 1_500);
        assert_eq!(annotation_box_end(&annotation, false, Some(10_000)), 1_500);
    }

    #[test]
    fn annotation_label_width_scales_with_hex_digits() {
        assert!(annotation_label_width("600081") > annotation_label_width("4F"));
        assert!(annotation_label_width("4F") >= 26.0);
    }

    #[test]
    fn short_annotations_have_a_readable_screen_width() {
        assert_eq!(annotation_right_x(20.0, 21.0), 28.0);
        assert_eq!(annotation_right_x(20.0, 35.0), 35.0);
    }

    #[test]
    fn clipped_annotation_uses_its_visible_width_for_the_label() {
        let wave_rect = Rect::from_min_max(Pos2::new(100.0, 0.0), Pos2::new(300.0, 20.0));
        let clipped_left = Rect::from_min_max(Pos2::new(50.0, 2.0), Pos2::new(180.0, 18.0));
        let clipped_right = Rect::from_min_max(Pos2::new(260.0, 2.0), Pos2::new(350.0, 18.0));
        let too_narrow = Rect::from_min_max(Pos2::new(290.0, 2.0), Pos2::new(350.0, 18.0));
        let label_width = annotation_label_width("27");

        assert_eq!(
            annotation_label_position(clipped_left, wave_rect, label_width),
            Some(Pos2::new(140.0, 10.0))
        );
        assert_eq!(
            annotation_label_position(clipped_right, wave_rect, label_width),
            Some(Pos2::new(280.0, 10.0))
        );
        assert_eq!(
            annotation_label_position(too_narrow, wave_rect, label_width),
            None
        );
    }

    #[test]
    fn open_annotation_that_is_not_the_last_one_ever_is_left_alone() {
        // Shouldn't happen in practice (append_word always patches the
        // second-to-last entry), but the fallback must not invent a box.
        let annotation = Annotation {
            start_ns: 1_000,
            end_ns: 1_000,
            value: 0x4F,
        };
        assert_eq!(annotation_box_end(&annotation, false, Some(10_000)), 1_000);
    }
}
