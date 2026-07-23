//! Built-in collected-payload renderers for `Viewer` subscriptions.

use std::sync::Arc;

use egui::Color32;

use logic_analyzer_viewer::{
    AnnotationVisual, DerivedLaneId, OpaqueLaneDrawContext, ViewerLaneGroup, ViewerLaneInteraction,
    ViewerLaneRenderer, ViewerLaneTrack, ViewerLaneTrackId, default_annotation_visual,
    draw_annotation_presence, draw_annotation_snapshot, draw_digital_activity,
    draw_digital_snapshot, draw_trigger_activity, draw_trigger_snapshot, draw_value_activity,
    draw_value_snapshot,
};
use signal_processing::{
    DigitalLaneSnapshot, NumberLaneSnapshot, OpaqueCollectedLaneSnapshot, TextLaneSnapshot,
    TriggerLaneSnapshot, WordLaneSnapshot,
};

/// Renders the built-in digital payload from its adapter-owned snapshot.
pub(crate) struct DigitalSnapshotRenderer;

impl ViewerLaneRenderer for DigitalSnapshotRenderer {
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

    fn interaction(
        &self,
        _track: &ViewerLaneTrack,
        snapshot: Option<&OpaqueCollectedLaneSnapshot>,
    ) -> Option<ViewerLaneInteraction> {
        let snapshot = snapshot?.value::<DigitalLaneSnapshot>()?;
        let (initial, transitions) = match snapshot.as_ref() {
            DigitalLaneSnapshot::Exact { samples, initial } => (
                *initial,
                samples
                    .iter()
                    .map(|sample| (sample.start_time_ns, sample.value))
                    .collect(),
            ),
            DigitalLaneSnapshot::Activity { records, initial } => {
                (*initial, digital_activity_transitions(records))
            }
        };
        Some(ViewerLaneInteraction {
            initial,
            transitions,
            event: false,
        })
    }
}

/// Renders the built-in trigger payload from its adapter-owned snapshot.
pub(crate) struct TriggerSnapshotRenderer;

impl ViewerLaneRenderer for TriggerSnapshotRenderer {
    fn draw_opaque_lane(
        &self,
        _track: &ViewerLaneTrack,
        snapshot: Option<&OpaqueCollectedLaneSnapshot>,
        context: OpaqueLaneDrawContext<'_>,
    ) -> bool {
        let Some(snapshot) = snapshot.and_then(|snapshot| snapshot.value::<TriggerLaneSnapshot>())
        else {
            return false;
        };
        match snapshot.as_ref() {
            TriggerLaneSnapshot::Exact(markers) => draw_trigger_snapshot(&context, markers),
            TriggerLaneSnapshot::Activity(records) => draw_trigger_activity(&context, records),
        }
        true
    }

    fn interaction(
        &self,
        _track: &ViewerLaneTrack,
        snapshot: Option<&OpaqueCollectedLaneSnapshot>,
    ) -> Option<ViewerLaneInteraction> {
        let snapshot = snapshot?.value::<TriggerLaneSnapshot>()?;
        let timestamps: Vec<u64> = match snapshot.as_ref() {
            TriggerLaneSnapshot::Exact(markers) => markers.clone(),
            TriggerLaneSnapshot::Activity(records) => {
                records.iter().map(|record| record.start_ns).collect()
            }
        };
        let mut value = false;
        let transitions = timestamps
            .into_iter()
            .map(|timestamp| {
                value = !value;
                (timestamp, value)
            })
            .collect();
        Some(ViewerLaneInteraction {
            initial: false,
            transitions,
            event: true,
        })
    }
}

fn digital_activity_transitions(records: &[signal_processing::MipmapRecord]) -> Vec<(u64, bool)> {
    let mut transitions = Vec::with_capacity(records.len().saturating_mul(2));
    for record in records {
        let Some((first, last)) = record.level_hint else {
            continue;
        };
        transitions.push((record.start_ns, first));
        if first != last {
            transitions.push((record.end_ns, last));
        }
    }
    transitions
}

/// Renders the built-in numeric-level payload from its typed snapshot.
pub(crate) struct NumberSnapshotRenderer;

impl ViewerLaneRenderer for NumberSnapshotRenderer {
    fn draw_opaque_lane(
        &self,
        _track: &ViewerLaneTrack,
        snapshot: Option<&OpaqueCollectedLaneSnapshot>,
        context: OpaqueLaneDrawContext<'_>,
    ) -> bool {
        let Some(snapshot) = snapshot.and_then(|snapshot| snapshot.value::<NumberLaneSnapshot>())
        else {
            return false;
        };
        let color = Color32::from_rgb(95, 145, 210);
        match snapshot.as_ref() {
            NumberLaneSnapshot::Exact(samples) => {
                let values = samples
                    .iter()
                    .map(|sample| (sample.start_time_ns, sample.value.to_string()))
                    .collect::<Vec<_>>();
                draw_value_snapshot(&context, &values, color);
            }
            NumberLaneSnapshot::Activity(records) => draw_value_activity(&context, records, color),
        }
        true
    }
}

/// Renders the built-in text-level payload from its typed snapshot.
pub(crate) struct TextSnapshotRenderer;

impl ViewerLaneRenderer for TextSnapshotRenderer {
    fn draw_opaque_lane(
        &self,
        _track: &ViewerLaneTrack,
        snapshot: Option<&OpaqueCollectedLaneSnapshot>,
        context: OpaqueLaneDrawContext<'_>,
    ) -> bool {
        let Some(snapshot) = snapshot.and_then(|snapshot| snapshot.value::<TextLaneSnapshot>())
        else {
            return false;
        };
        let color = Color32::from_rgb(215, 150, 170);
        match snapshot.as_ref() {
            TextLaneSnapshot::Exact(samples) => {
                let values = samples
                    .iter()
                    .map(|sample| (sample.start_time_ns, sample.value.clone()))
                    .collect::<Vec<_>>();
                draw_value_snapshot(&context, &values, color);
            }
            TextLaneSnapshot::Activity(records) => draw_value_activity(&context, records, color),
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
    use signal_processing::Sample;

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

    #[test]
    fn digital_snapshot_projects_payload_neutral_interaction() {
        let renderer = DigitalSnapshotRenderer;
        let track = ViewerLaneTrack::new("signal", DerivedLaneId::new("signal"), 1.0);
        let snapshot = OpaqueCollectedLaneSnapshot::new(Arc::new(DigitalLaneSnapshot::Exact {
            samples: vec![Sample::new(true, 10), Sample::new(false, 20)],
            initial: false,
        }));

        assert_eq!(
            renderer.interaction(&track, Some(&snapshot)),
            Some(ViewerLaneInteraction {
                initial: false,
                transitions: vec![(10, true), (20, false)],
                event: false,
            })
        );
    }

    #[test]
    fn trigger_snapshot_projects_event_interaction() {
        let renderer = TriggerSnapshotRenderer;
        let track = ViewerLaneTrack::new("trigger", DerivedLaneId::new("trigger"), 1.0);
        let snapshot =
            OpaqueCollectedLaneSnapshot::new(Arc::new(TriggerLaneSnapshot::Exact(vec![
                10, 20, 30,
            ])));

        assert_eq!(
            renderer.interaction(&track, Some(&snapshot)),
            Some(ViewerLaneInteraction {
                initial: false,
                transitions: vec![(10, true), (20, false), (30, true)],
                event: true,
            })
        );
    }
}
