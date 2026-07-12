use crate::types::AnalyzerLayout;
use crate::viewer::LogicAnalyzerViewer;
use dsl::runtime::derived_word_store::{AnnotationQuery, WordPresenceBucket};
use dsl::{Annotation, DerivedLaneData};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IndexedAnnotationCacheKey {
    query_id: usize,
    generation: u64,
    start_ns: u64,
    end_ns: u64,
    target_points: usize,
}

#[derive(Debug, Clone)]
pub(crate) enum IndexedAnnotationSamples {
    Exact {
        annotations: Vec<Annotation>,
        last_timestamp_ns: Option<u64>,
    },
    Presence(Vec<WordPresenceBucket>),
    Error,
}

#[derive(Debug, Clone)]
pub(crate) struct IndexedAnnotationCacheEntry {
    key: IndexedAnnotationCacheKey,
    pub(crate) samples: IndexedAnnotationSamples,
}

impl LogicAnalyzerViewer {
    /// Refreshes indexed word lanes for the current viewport. Only query
    /// handles are cloned while `DerivedLanes` is locked; file access and
    /// decoding happen after the guard has been released.
    pub(crate) fn sample_indexed_annotations(&mut self, layout: AnalyzerLayout) {
        if layout.wave_rect.width() <= 1.0 {
            return;
        }
        let Some(derived) = self.derived.as_ref() else {
            self.indexed_annotation_cache.clear();
            return;
        };
        let queries: Vec<(String, Arc<dyn AnnotationQuery>)> = {
            let lanes = derived.read();
            lanes
                .iter()
                .filter_map(|lane| match &lane.data {
                    DerivedLaneData::IndexedAnnotations(indexed) => {
                        Some((lane.name.clone(), Arc::clone(&indexed.query)))
                    }
                    _ => None,
                })
                .collect()
        };
        let active: HashSet<&str> = queries.iter().map(|(name, _)| name.as_str()).collect();
        self.indexed_annotation_cache
            .retain(|name, _| active.contains(name.as_str()));

        let (start_ns, end_ns) = self.visible_window_ns();
        let target_points = layout.wave_rect.width().max(1.0).round() as usize;
        let exact_limit = target_points.saturating_mul(2).max(32);
        for (name, query) in queries {
            let metadata = query.metadata();
            let key = IndexedAnnotationCacheKey {
                query_id: Arc::as_ptr(&query) as *const () as usize,
                generation: metadata.generation,
                start_ns,
                end_ns,
                target_points,
            };
            if self
                .indexed_annotation_cache
                .get(&name)
                .is_some_and(|entry| entry.key == key)
            {
                continue;
            }

            let samples = match query.exact_window(start_ns, end_ns, exact_limit) {
                Ok(window) if window.complete => IndexedAnnotationSamples::Exact {
                    annotations: window.annotations,
                    last_timestamp_ns: metadata.last_timestamp_ns,
                },
                Ok(_) => match query.presence_window(start_ns, end_ns, target_points) {
                    Ok(buckets) => IndexedAnnotationSamples::Presence(buckets),
                    Err(_) => IndexedAnnotationSamples::Error,
                },
                Err(_) => IndexedAnnotationSamples::Error,
            };
            self.indexed_annotation_cache
                .insert(name, IndexedAnnotationCacheEntry { key, samples });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AnalyzerLayout;
    use dsl::runtime::derived_word_store::{IndexedAnnotationWriter, LiveStoreConfig};
    use dsl::{DerivedLaneData, DerivedLanes, IndexedAnnotationLane, Word};
    use egui::{Pos2, Rect};

    fn layout(width: f32) -> AnalyzerLayout {
        let empty = Rect::NOTHING;
        AnalyzerLayout {
            header_rect: empty,
            ruler_rect: empty,
            labels_rect: empty,
            wave_rect: Rect::from_min_max(Pos2::ZERO, Pos2::new(width, 30.0)),
            row_height: 30.0,
            name_col_width: 0.0,
            badge_width: 0.0,
        }
    }

    fn indexed_viewer(words: &[Word]) -> LogicAnalyzerViewer {
        let mut config = LiveStoreConfig::default();
        config.block.max_words = 8;
        let (mut writer, store) = IndexedAnnotationWriter::create(config).unwrap();
        writer.append_batch(words).unwrap();
        writer.finish().unwrap();
        let lanes = DerivedLanes::new();
        lanes.register(
            "words",
            DerivedLaneData::IndexedAnnotations(IndexedAnnotationLane::from_store(store)),
        );
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.set_derived_lanes(lanes);
        viewer
    }

    #[test]
    fn old_committed_words_are_sampled_exactly_with_explicit_duration() {
        let words: Vec<_> = (0..24)
            .map(|index| {
                if index == 3 {
                    Word::spanning(index, index * 1_000, 400)
                } else {
                    Word::new(index, index * 1_000)
                }
            })
            .collect();
        let mut viewer = indexed_viewer(&words);
        viewer.visible_start_us = 2.0;
        viewer.visible_span_us = 3.0;
        viewer.sample_indexed_annotations(layout(1_000.0));

        let entry = viewer.indexed_annotation_cache.get("words").unwrap();
        let IndexedAnnotationSamples::Exact { annotations, .. } = &entry.samples else {
            panic!("expected exact indexed annotations");
        };
        let partial = annotations
            .iter()
            .find(|annotation| annotation.value == 3)
            .unwrap();
        assert_eq!(partial.start_ns, 3_000);
        assert_eq!(partial.end_ns, 3_400);
    }

    #[test]
    fn dense_window_uses_bounded_presence_buckets() {
        let words: Vec<_> = (0..10_000)
            .map(|index| Word::new(index, index * 10))
            .collect();
        let mut viewer = indexed_viewer(&words);
        viewer.visible_span_us = 100.0;
        viewer.sample_indexed_annotations(layout(100.0));

        let entry = viewer.indexed_annotation_cache.get("words").unwrap();
        let IndexedAnnotationSamples::Presence(buckets) = &entry.samples else {
            panic!("expected indexed presence buckets");
        };
        assert!(!buckets.is_empty());
        assert!(buckets.len() <= 100);
        assert!(buckets.iter().all(|bucket| bucket.word_count > 0));
        assert_eq!(buckets.first().unwrap().start_ns, 0);
        assert!(buckets.last().unwrap().end_ns >= 99_990);
    }

    #[test]
    fn live_generation_refreshes_the_cached_exact_window() {
        let config = LiveStoreConfig {
            hot_tail_publish_words: 1,
            ..LiveStoreConfig::default()
        };
        let (mut writer, store) = IndexedAnnotationWriter::create(config).unwrap();
        let lanes = DerivedLanes::new();
        lanes.register(
            "words",
            DerivedLaneData::IndexedAnnotations(IndexedAnnotationLane::from_store(store)),
        );
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.set_derived_lanes(lanes);

        writer.append(Word::new(1, 1_000)).unwrap();
        viewer.sample_indexed_annotations(layout(1_000.0));
        let first_generation = viewer.indexed_annotation_cache["words"].key.generation;

        writer.append(Word::new(2, 2_000)).unwrap();
        viewer.sample_indexed_annotations(layout(1_000.0));
        let entry = &viewer.indexed_annotation_cache["words"];
        assert!(entry.key.generation > first_generation);
        let IndexedAnnotationSamples::Exact { annotations, .. } = &entry.samples else {
            panic!("expected exact indexed annotations");
        };
        assert_eq!(annotations.last().unwrap().value, 2);
    }
}
