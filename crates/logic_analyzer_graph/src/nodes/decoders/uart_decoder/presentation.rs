//! Viewer presentation for UART-derived lanes.

use std::sync::Arc;

use egui::{Color32, Stroke};

use logic_analyzer_viewer::{
    AnnotationVisual, DerivedLaneId, ViewerLaneBadge, ViewerLaneGroup, ViewerLaneRenderer,
    ViewerLaneTheme, ViewerLaneTrackId, ViewerOutputPresentation,
};

use crate::collected_payloads::WordSnapshotRenderer;
use crate::decoder_table::{DecoderTableCellMode, DecoderTableColumnPresentation};

const START: u64 = u64::MAX;
const STOP: u64 = u64::MAX - 1;
const ERROR: u64 = u64::MAX - 2;

struct UartLaneRenderer;

impl ViewerLaneRenderer for UartLaneRenderer {
    fn row_height(&self, group: &ViewerLaneGroup, base_height: f32) -> f32 {
        if group.tracks.len() > 1 {
            base_height * 3.0
        } else {
            base_height
        }
    }

    fn annotation_visual(
        &self,
        track: &ViewerLaneTrackId,
        theme: &ViewerLaneTheme,
        value: u64,
        mut default: AnnotationVisual,
    ) -> AnnotationVisual {
        if track.as_str() == "bits" && value <= 1 {
            default.label = value.to_string();
        } else if track.as_str() == "frame" {
            match value {
                START => default.label = "S".to_owned(),
                STOP => default.label = "T".to_owned(),
                ERROR => {
                    default.label = "Error".to_owned();
                    default.fill = theme.error.gamma_multiply(0.35);
                    default.border = Stroke::new(1.0, theme.error);
                }
                _ => {}
            }
        }
        default
    }

    fn snap_lanes(&self, group: &ViewerLaneGroup, _pointer_fraction: f32) -> Vec<DerivedLaneId> {
        group
            .tracks
            .iter()
            .map(|track| track.lane.clone())
            .collect()
    }
}

pub(crate) fn uart_output_presentation(def_index: usize) -> Option<ViewerOutputPresentation> {
    let renderer: Arc<dyn ViewerLaneRenderer> =
        Arc::new(WordSnapshotRenderer::new(Arc::new(UartLaneRenderer)));
    let badge = ViewerLaneBadge::new("W", Color32::from_rgb(215, 140, 60));
    match def_index {
        2 => Some(ViewerOutputPresentation::new(
            "frame", "bits", 0, 1.0, badge, renderer,
        )),
        3 => Some(ViewerOutputPresentation::new(
            "frame", "frame", 1, 1.0, badge, renderer,
        )),
        _ => None,
    }
}

pub(crate) fn uart_table_column(def_index: usize) -> Option<DecoderTableColumnPresentation> {
    let (column_key, label, order, row_anchor, mode, track_key) = match def_index {
        2 => (
            "bits",
            "Bits",
            0,
            false,
            DecoderTableCellMode::Joined(String::new()),
            "bits",
        ),
        3 => (
            "frame",
            "Data",
            1,
            true,
            DecoderTableCellMode::Single,
            "frame",
        ),
        _ => return None,
    };
    Some(DecoderTableColumnPresentation::new(
        "decoder",
        column_key,
        label,
        order,
        row_anchor,
        mode,
        track_key,
        Arc::new(UartLaneRenderer),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visual(label: &str) -> AnnotationVisual {
        AnnotationVisual {
            label: label.to_owned(),
            fill: Color32::BLACK,
            border: Stroke::new(1.0, Color32::WHITE),
        }
    }

    #[test]
    fn uart_semantics_are_owned_by_the_uart_renderer() {
        let renderer = UartLaneRenderer;
        let bits = ViewerLaneTrackId::new("bits");
        let frame = ViewerLaneTrackId::new("frame");
        let theme =
            ViewerLaneTheme::from_visuals(&egui::Visuals::dark(), Color32::from_rgb(215, 140, 60));

        assert_eq!(
            renderer
                .annotation_visual(&bits, &theme, 1, visual("0x1"))
                .label,
            "1"
        );
        assert_eq!(
            renderer
                .annotation_visual(&frame, &theme, START, visual("default"))
                .label,
            "S"
        );
        assert_eq!(
            renderer
                .annotation_visual(&frame, &theme, STOP, visual("default"))
                .label,
            "T"
        );
        let error = renderer.annotation_visual(&frame, &theme, ERROR, visual("default"));
        assert_eq!(error.label, "Error");
        assert_eq!(error.fill, theme.error.gamma_multiply(0.35));
    }

    #[test]
    fn only_uart_detail_outputs_join_the_compound_group() {
        assert!(uart_output_presentation(0).is_none());
        assert!(uart_output_presentation(1).is_none());
        assert_eq!(uart_output_presentation(2).unwrap().track_key, "bits");
        assert_eq!(uart_output_presentation(3).unwrap().track_key, "frame");
    }

    #[test]
    fn partial_uart_group_keeps_default_height() {
        let renderer: Arc<dyn ViewerLaneRenderer> = Arc::new(UartLaneRenderer);
        let mut group = ViewerLaneGroup {
            id: logic_analyzer_viewer::ViewerLaneGroupId::new("uart"),
            label: "Serial".to_owned(),
            badge: ViewerLaneBadge::new("W", Color32::WHITE),
            tracks: vec![logic_analyzer_viewer::ViewerLaneTrack::new(
                "frame",
                DerivedLaneId::new("frame"),
                1.0,
            )],
            renderer: Arc::clone(&renderer),
        };

        assert_eq!(renderer.row_height(&group, 30.0), 30.0);
        group
            .tracks
            .push(logic_analyzer_viewer::ViewerLaneTrack::new(
                "bits",
                DerivedLaneId::new("bits"),
                1.0,
            ));
        assert_eq!(renderer.row_height(&group, 30.0), 90.0);
    }
}
