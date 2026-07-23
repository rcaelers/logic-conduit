//! Built-in collected-payload renderers for `Viewer` subscriptions.

use std::sync::Arc;

use egui::Color32;

use logic_analyzer_viewer::{
    AnnotationVisual, DerivedLaneId, OpaqueLaneDrawContext, ViewerLaneGroup, ViewerLaneRenderer,
    ViewerLaneTrack, ViewerLaneTrackId, default_annotation_visual, draw_annotation_presence,
    draw_annotation_snapshot, draw_digital_activity, draw_digital_snapshot,
};
use signal_processing::{DigitalLaneSnapshot, OpaqueCollectedLaneSnapshot, WordLaneSnapshot};

/// Renders the built-in digital payload from its adapter-owned snapshot.
pub(crate) struct DigitalSnapshotRenderer;

impl ViewerLaneRenderer for DigitalSnapshotRenderer {
    fn uses_opaque_snapshot(&self, _track: &ViewerLaneTrack) -> bool {
        true
    }

    fn draw_opaque_lane(
        &self,
        _track: &ViewerLaneTrack,
        snapshot: Option<&OpaqueCollectedLaneSnapshot>,
        context: OpaqueLaneDrawContext<'_>,
    ) -> bool {
        let Some(snapshot) = snapshot.and_then(|snapshot| snapshot.value::<DigitalLaneSnapshot>())
        else {
            return false;
        };
        match snapshot.as_ref() {
            DigitalLaneSnapshot::Exact { samples, initial } => {
                draw_digital_snapshot(&context, samples, *initial)
            }
            DigitalLaneSnapshot::Activity { records, initial } => {
                draw_digital_activity(&context, records, *initial)
            }
        }
        true
    }
}

/// Renders the built-in word payload from its adapter-owned snapshot while
/// delegating protocol-specific labels and snapping to the producer's
/// presentation renderer.
pub(crate) struct WordSnapshotRenderer {
    semantics: Arc<dyn ViewerLaneRenderer>,
}

impl WordSnapshotRenderer {
    pub(crate) fn new(semantics: Arc<dyn ViewerLaneRenderer>) -> Self {
        Self { semantics }
    }
}

impl ViewerLaneRenderer for WordSnapshotRenderer {
    fn row_height(&self, group: &ViewerLaneGroup, base_height: f32) -> f32 {
        self.semantics.row_height(group, base_height)
    }

    fn annotation_visual(
        &self,
        track: &ViewerLaneTrackId,
        value: u64,
        default: AnnotationVisual,
    ) -> AnnotationVisual {
        self.semantics.annotation_visual(track, value, default)
    }

    fn uses_opaque_snapshot(&self, _track: &ViewerLaneTrack) -> bool {
        true
    }

    fn draw_opaque_lane(
        &self,
        track: &ViewerLaneTrack,
        snapshot: Option<&OpaqueCollectedLaneSnapshot>,
        context: OpaqueLaneDrawContext<'_>,
    ) -> bool {
        let Some(snapshot) = snapshot.and_then(|snapshot| snapshot.value::<WordLaneSnapshot>())
        else {
            return false;
        };
        match snapshot.as_ref() {
            WordLaneSnapshot::Exact {
                annotations,
                last_timestamp_ns,
                display_format,
            } => draw_annotation_snapshot(&context, annotations, *last_timestamp_ns, |value| {
                let default = default_annotation_visual(value, display_format.as_deref());
                self.semantics.annotation_visual(&track.id, value, default)
            }),
            WordLaneSnapshot::Presence(buckets) => draw_annotation_presence(
                &context,
                buckets
                    .iter()
                    .map(|bucket| (bucket.start_ns, bucket.end_ns, bucket.word_count)),
            ),
            WordLaneSnapshot::Activity => {
                let top = context.top + context.height * 0.12;
                let bottom = context.top + context.height * 0.88;
                context.painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::Pos2::new(context.wave_rect.left(), top),
                        egui::Pos2::new(context.wave_rect.right(), bottom),
                    ),
                    0.0,
                    Color32::from_rgb(215, 140, 60),
                );
            }
            WordLaneSnapshot::Error => return false,
        }
        true
    }

    fn snap_lanes(&self, group: &ViewerLaneGroup, pointer_fraction: f32) -> Vec<DerivedLaneId> {
        self.semantics.snap_lanes(group, pointer_fraction)
    }
}

#[cfg(test)]
mod presentation_tests {
    use egui::{Color32, Stroke};

    use super::*;

    struct SemanticRenderer;

    impl ViewerLaneRenderer for SemanticRenderer {
        fn annotation_visual(
            &self,
            _track: &ViewerLaneTrackId,
            value: u64,
            mut default: AnnotationVisual,
        ) -> AnnotationVisual {
            default.label = format!("semantic-{value}");
            default
        }
    }

    #[test]
    fn word_snapshot_renderer_requests_snapshots_and_delegates_semantics() {
        let renderer = WordSnapshotRenderer::new(Arc::new(SemanticRenderer));
        let track = ViewerLaneTrack::new("data", DerivedLaneId::new("words"), 1.0);
        assert!(renderer.uses_opaque_snapshot(&track));
        let default = AnnotationVisual {
            label: "default".to_owned(),
            fill: Color32::BLACK,
            border: Stroke::new(1.0, Color32::WHITE),
        };
        assert_eq!(
            renderer.annotation_visual(&track.id, 42, default).label,
            "semantic-42"
        );
    }
}
