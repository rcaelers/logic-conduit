use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, RwLock, RwLockReadGuard};

use egui::{Color32, Stroke};

/// Explicit identity of one payload in [`signal_processing::DerivedLanes`].
///
/// The current runtime store uses its lane name as its stable key. Wrapping
/// it prevents presentation code from treating that key as display text or
/// inferring behavior from it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DerivedLaneId(String);

impl DerivedLaneId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewerLaneGroupId(String);

impl ViewerLaneGroupId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewerLaneTrackId(String);

impl ViewerLaneTrackId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct ViewerLaneBadge {
    pub text: String,
    pub color: Color32,
}

impl ViewerLaneBadge {
    pub fn new(text: impl Into<String>, color: Color32) -> Self {
        Self {
            text: text.into(),
            color,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ViewerLaneTrack {
    pub id: ViewerLaneTrackId,
    pub lane: DerivedLaneId,
    pub relative_height: f32,
}

impl ViewerLaneTrack {
    pub fn new(id: impl Into<String>, lane: DerivedLaneId, relative_height: f32) -> Self {
        Self {
            id: ViewerLaneTrackId::new(id),
            lane,
            relative_height: relative_height.max(0.25),
        }
    }
}

/// Bounded semantic snapshot prepared for one track in the visible window.
/// Renderer/plugin code receives values only from exact sparse frames;
/// dense frames are represented as activity and require no value formatting.
#[derive(Debug, Clone)]
pub struct ViewerLaneTrackFrame {
    pub track: ViewerLaneTrackId,
    pub annotation_values: Vec<u64>,
    pub dense: bool,
}

/// Per-row frame assembled while the runtime lane store is locked and used
/// only after that lock has been released.
#[derive(Debug, Clone, Default)]
pub struct ViewerLaneFrame {
    pub tracks: Vec<ViewerLaneTrackFrame>,
}

/// Fully resolved visual properties for one annotation box.
#[derive(Debug, Clone)]
pub struct AnnotationVisual {
    pub label: String,
    pub fill: Color32,
    pub border: Stroke,
}

/// Protocol-neutral extension point for a displayed derived-lane row.
///
/// The viewer retains ownership of waveform queries and drawing geometry.
/// Concrete renderers select row sizing, annotation semantics, and which
/// explicitly registered tracks participate in cursor snapping.
pub trait ViewerLaneRenderer: Send + Sync {
    fn row_height(&self, group: &ViewerLaneGroup, base_height: f32) -> f32 {
        let weight = group
            .tracks
            .iter()
            .map(|track| track.relative_height)
            .sum::<f32>()
            .max(1.0);
        base_height * weight
    }

    fn annotation_visual(
        &self,
        _track: &ViewerLaneTrackId,
        _value: u64,
        default: AnnotationVisual,
    ) -> AnnotationVisual {
        default
    }

    fn snap_lanes(&self, group: &ViewerLaneGroup, pointer_fraction: f32) -> Vec<DerivedLaneId> {
        let total = group
            .tracks
            .iter()
            .map(|track| track.relative_height)
            .sum::<f32>()
            .max(1.0);
        let target = pointer_fraction.clamp(0.0, 1.0) * total;
        let mut top = 0.0;
        group
            .tracks
            .iter()
            .find(|track| {
                let contains = target >= top && target <= top + track.relative_height;
                top += track.relative_height;
                contains
            })
            .map(|track| vec![track.lane.clone()])
            .unwrap_or_default()
    }
}

#[derive(Default)]
pub struct DefaultViewerLaneRenderer;

impl ViewerLaneRenderer for DefaultViewerLaneRenderer {}

pub struct ViewerLaneGroup {
    pub id: ViewerLaneGroupId,
    pub label: String,
    pub badge: ViewerLaneBadge,
    pub tracks: Vec<ViewerLaneTrack>,
    pub renderer: Arc<dyn ViewerLaneRenderer>,
}

impl fmt::Debug for ViewerLaneGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ViewerLaneGroup")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("badge", &self.badge)
            .field("tracks", &self.tracks)
            .finish_non_exhaustive()
    }
}

impl Clone for ViewerLaneGroup {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            label: self.label.clone(),
            badge: self.badge.clone(),
            tracks: self.tracks.clone(),
            renderer: Arc::clone(&self.renderer),
        }
    }
}

impl ViewerLaneGroup {
    pub fn singleton(
        id: ViewerLaneGroupId,
        label: impl Into<String>,
        badge: ViewerLaneBadge,
        lane: DerivedLaneId,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            badge,
            tracks: vec![ViewerLaneTrack::new("primary", lane, 1.0)],
            renderer: Arc::new(DefaultViewerLaneRenderer),
        }
    }

    pub fn track_rects(&self, top: f32, height: f32) -> Vec<(ViewerLaneTrack, f32, f32)> {
        let total = self
            .tracks
            .iter()
            .map(|track| track.relative_height)
            .sum::<f32>()
            .max(1.0);
        let mut cursor = top;
        self.tracks
            .iter()
            .map(|track| {
                let track_height = height * track.relative_height / total;
                let result = (track.clone(), cursor, track_height);
                cursor += track_height;
                result
            })
            .collect()
    }
}

/// Metadata supplied by a producer builder for one output socket.
#[derive(Clone)]
pub struct ViewerOutputPresentation {
    pub group_key: String,
    pub track_key: String,
    pub track_order: usize,
    pub relative_height: f32,
    pub badge: ViewerLaneBadge,
    pub renderer: Arc<dyn ViewerLaneRenderer>,
}

impl fmt::Debug for ViewerOutputPresentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ViewerOutputPresentation")
            .field("group_key", &self.group_key)
            .field("track_key", &self.track_key)
            .field("track_order", &self.track_order)
            .field("relative_height", &self.relative_height)
            .field("badge", &self.badge)
            .finish_non_exhaustive()
    }
}

impl ViewerOutputPresentation {
    pub fn new(
        group_key: impl Into<String>,
        track_key: impl Into<String>,
        track_order: usize,
        relative_height: f32,
        badge: ViewerLaneBadge,
        renderer: Arc<dyn ViewerLaneRenderer>,
    ) -> Self {
        Self {
            group_key: group_key.into(),
            track_key: track_key.into(),
            track_order,
            relative_height,
            badge,
            renderer,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ViewerLaneRegistry {
    inner: Arc<RwLock<Vec<ViewerLaneGroup>>>,
}

impl ViewerLaneRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, group: ViewerLaneGroup) {
        let claimed: HashSet<&DerivedLaneId> =
            group.tracks.iter().map(|track| &track.lane).collect();
        let mut groups = self.inner.write().unwrap();
        groups.retain(|existing| {
            existing.id == group.id
                || existing
                    .tracks
                    .iter()
                    .all(|track| !claimed.contains(&track.lane))
        });
        if let Some(existing) = groups.iter_mut().find(|existing| existing.id == group.id) {
            *existing = group;
        } else {
            groups.push(group);
        }
    }

    pub fn read(&self) -> RwLockReadGuard<'_, Vec<ViewerLaneGroup>> {
        self.inner.read().unwrap()
    }

    pub fn clear(&self) {
        self.inner.write().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_registration_replaces_overlapping_singletons() {
        let registry = ViewerLaneRegistry::new();
        let first = DerivedLaneId::new("first");
        let second = DerivedLaneId::new("second");
        let badge = ViewerLaneBadge::new("W", Color32::WHITE);
        registry.register(ViewerLaneGroup::singleton(
            ViewerLaneGroupId::new("first-row"),
            "First",
            badge.clone(),
            first.clone(),
        ));
        registry.register(ViewerLaneGroup::singleton(
            ViewerLaneGroupId::new("second-row"),
            "Second",
            badge.clone(),
            second.clone(),
        ));

        registry.register(ViewerLaneGroup {
            id: ViewerLaneGroupId::new("compound"),
            label: "Compound".to_owned(),
            badge,
            tracks: vec![
                ViewerLaneTrack::new("a", first, 1.0),
                ViewerLaneTrack::new("b", second, 1.0),
            ],
            renderer: Arc::new(DefaultViewerLaneRenderer),
        });

        let groups = registry.read();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].id.as_str(), "compound");
        assert_eq!(groups[0].tracks.len(), 2);
    }

    #[test]
    fn default_renderer_snaps_only_the_track_under_the_pointer() {
        let first = DerivedLaneId::new("first");
        let second = DerivedLaneId::new("second");
        let group = ViewerLaneGroup {
            id: ViewerLaneGroupId::new("compound"),
            label: "Compound".to_owned(),
            badge: ViewerLaneBadge::new("W", Color32::WHITE),
            tracks: vec![
                ViewerLaneTrack::new("a", first.clone(), 1.0),
                ViewerLaneTrack::new("b", second.clone(), 1.0),
            ],
            renderer: Arc::new(DefaultViewerLaneRenderer),
        };

        assert_eq!(group.renderer.snap_lanes(&group, 0.2), vec![first]);
        assert_eq!(group.renderer.snap_lanes(&group, 0.8), vec![second]);
    }
}
