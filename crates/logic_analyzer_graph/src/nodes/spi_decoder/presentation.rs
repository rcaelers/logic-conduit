//! Viewer presentation for SPI-derived lanes.

use std::sync::Arc;

use egui::Color32;

use logic_analyzer_viewer::{
    AnnotationVisual, DerivedLaneId, ViewerLaneBadge, ViewerLaneGroup, ViewerLaneRenderer,
    ViewerLaneTrackId, ViewerOutputPresentation,
};

struct SpiLaneRenderer;

impl ViewerLaneRenderer for SpiLaneRenderer {
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
    let renderer: Arc<dyn ViewerLaneRenderer> = Arc::new(SpiLaneRenderer);
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
        assert_eq!(spi_output_presentation(2).unwrap().group_key, "mosi");
        assert_eq!(spi_output_presentation(3).unwrap().track_key, "data");
        assert_eq!(spi_output_presentation(4).unwrap().group_key, "miso");
        assert_eq!(spi_output_presentation(5).unwrap().track_key, "data");
    }
}
