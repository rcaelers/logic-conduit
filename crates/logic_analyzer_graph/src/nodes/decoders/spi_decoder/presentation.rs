//! Viewer presentation for SPI-derived lanes.

use std::sync::Arc;

use egui::Color32;

use logic_analyzer_viewer::{
    AnnotationVisual, DerivedLaneId, ViewerLaneBadge, ViewerLaneGroup, ViewerLaneRenderer,
    ViewerLaneTrackId, ViewerOutputPresentation,
};

use crate::decoder_table::{DecoderTableCellMode, DecoderTableColumnPresentation};
use crate::nodes::sinks::WordSnapshotRenderer;

struct SpiLaneRenderer;

impl ViewerLaneRenderer for SpiLaneRenderer {
    fn annotation_visual(
        &self,
        track: &ViewerLaneTrackId,
        value: u64,
        mut default: AnnotationVisual,
    ) -> AnnotationVisual {
        if track.as_str() == "bits" && value <= 1 {
            default.label = value.to_string();
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

pub(crate) fn spi_output_presentation(def_index: usize) -> Option<ViewerOutputPresentation> {
    let renderer: Arc<dyn ViewerLaneRenderer> =
        Arc::new(WordSnapshotRenderer::new(Arc::new(SpiLaneRenderer)));
    match def_index {
        2 => Some(ViewerOutputPresentation::new(
            "mosi",
            "bits",
            0,
            1.0,
            ViewerLaneBadge::new("O", Color32::from_rgb(215, 140, 60)),
            renderer,
        )),
        3 => Some(ViewerOutputPresentation::new(
            "mosi",
            "data",
            1,
            1.0,
            ViewerLaneBadge::new("O", Color32::from_rgb(215, 140, 60)),
            renderer,
        )),
        4 => Some(ViewerOutputPresentation::new(
            "miso",
            "bits",
            0,
            1.0,
            ViewerLaneBadge::new("I", Color32::from_rgb(90, 145, 210)),
            renderer,
        )),
        5 => Some(ViewerOutputPresentation::new(
            "miso",
            "data",
            1,
            1.0,
            ViewerLaneBadge::new("I", Color32::from_rgb(90, 145, 210)),
            renderer,
        )),
        _ => None,
    }
}

pub(crate) fn spi_table_column(def_index: usize) -> Option<DecoderTableColumnPresentation> {
    let (column_key, label, order, row_anchor, mode, track_key) = match def_index {
        2 => (
            "mosi_bits",
            "MOSI Bits",
            0,
            false,
            DecoderTableCellMode::Joined(String::new()),
            "bits",
        ),
        3 => (
            "mosi_data",
            "MOSI Data",
            1,
            true,
            DecoderTableCellMode::Single,
            "data",
        ),
        4 => (
            "miso_bits",
            "MISO Bits",
            2,
            false,
            DecoderTableCellMode::Joined(String::new()),
            "bits",
        ),
        5 => (
            "miso_data",
            "MISO Data",
            3,
            true,
            DecoderTableCellMode::Single,
            "data",
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
        Arc::new(SpiLaneRenderer),
    ))
}

#[cfg(test)]
mod tests {
    use egui::Stroke;

    use super::*;

    #[test]
    fn spi_bit_values_use_binary_labels() {
        let renderer = SpiLaneRenderer;
        let visual = AnnotationVisual {
            label: "0x1".to_owned(),
            fill: Color32::BLACK,
            border: Stroke::new(1.0, Color32::WHITE),
        };

        assert_eq!(
            renderer
                .annotation_visual(&ViewerLaneTrackId::new("bits"), 1, visual)
                .label,
            "1"
        );
    }

    #[test]
    fn detail_outputs_form_one_group_per_spi_direction() {
        assert!(spi_output_presentation(0).is_none());
        assert!(spi_output_presentation(1).is_none());
        let mosi_bits = spi_output_presentation(2).unwrap();
        let mosi_data = spi_output_presentation(3).unwrap();
        assert_eq!(mosi_bits.group_key, "mosi");
        assert_eq!(mosi_data.track_key, "data");
        assert_eq!(mosi_bits.relative_height, 1.0);
        assert_eq!(mosi_data.relative_height, 1.0);
        assert_eq!(spi_output_presentation(4).unwrap().group_key, "miso");
        assert_eq!(spi_output_presentation(5).unwrap().track_key, "data");
    }

    #[test]
    fn bits_and_data_each_use_one_standard_lane_height() {
        let renderer: Arc<dyn ViewerLaneRenderer> = Arc::new(SpiLaneRenderer);
        let group = ViewerLaneGroup {
            id: logic_analyzer_viewer::ViewerLaneGroupId::new("spi"),
            label: "SPI".to_owned(),
            badge: ViewerLaneBadge::new("O", Color32::WHITE),
            tracks: vec![
                logic_analyzer_viewer::ViewerLaneTrack::new(
                    "bits",
                    DerivedLaneId::new("bits"),
                    1.0,
                ),
                logic_analyzer_viewer::ViewerLaneTrack::new(
                    "data",
                    DerivedLaneId::new("data"),
                    1.0,
                ),
            ],
            renderer: Arc::clone(&renderer),
        };

        assert_eq!(renderer.row_height(&group, 30.0), 60.0);
        let rects = group.track_rects(0.0, 60.0);
        assert_eq!(rects[0].2, 30.0);
        assert_eq!(rects[1].2, 30.0);
    }
}
