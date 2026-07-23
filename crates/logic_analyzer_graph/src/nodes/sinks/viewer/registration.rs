//! Built-in collected-payload subscription registration.

use std::sync::Arc;

use egui::Color32;

use logic_analyzer_viewer::{DefaultViewerLaneRenderer, ViewerLaneBadge};
use signal_processing::{CollectedWordLaneOptions, LiveStoreConfig};

use super::presentation::{
    DigitalSnapshotRenderer, NumberSnapshotRenderer, TextSnapshotRenderer, TriggerSnapshotRenderer,
    WordSnapshotRenderer,
};
use crate::{BuilderRegistry, DefaultViewerPayloadPresentation};

pub(crate) fn register_collected_payload_subscriptions(registry: &mut BuilderRegistry) {
    registry
        .register_collected_payload_subscription::<signal_processing::Sample>(
            DefaultViewerPayloadPresentation::with_renderer(
                ViewerLaneBadge::new("S", Color32::from_rgb(95, 175, 95)),
                Arc::new(DigitalSnapshotRenderer),
            ),
        )
        .expect("built-in digital payload subscription must be valid");
    registry
        .register_collected_payload_subscription_with_request_configurator::<
            signal_processing::Word,
        >(
            DefaultViewerPayloadPresentation::with_renderer(
                ViewerLaneBadge::new("W", Color32::from_rgb(215, 140, 60)),
                Arc::new(WordSnapshotRenderer::new(Arc::new(
                    DefaultViewerLaneRenderer,
                ))),
            ),
            Arc::new(|request, member, input, ctx| {
                let store_config = if let Some(persistent) = ctx.derived_word_cache(member) {
                    LiveStoreConfig {
                        directory: persistent.directory.clone(),
                        persistence: Some(persistent.clone()),
                        ..LiveStoreConfig::default()
                    }
                } else {
                    LiveStoreConfig::default()
                };
                request.with_options(CollectedWordLaneOptions::new(
                    store_config,
                    input.word_display_format.clone(),
                ))
            }),
            true,
        )
        .expect("built-in word payload subscription must be valid");
    registry
        .register_collected_payload_subscription::<signal_processing::Trigger>(
            DefaultViewerPayloadPresentation::with_renderer(
                ViewerLaneBadge::new("T", Color32::from_rgb(230, 190, 80)),
                Arc::new(TriggerSnapshotRenderer),
            ),
        )
        .expect("built-in trigger payload subscription must be valid");
    registry
        .register_collected_payload_subscription::<signal_processing::NumberSample>(
            DefaultViewerPayloadPresentation::with_renderer(
                ViewerLaneBadge::new("N", Color32::from_rgb(95, 145, 210)),
                Arc::new(NumberSnapshotRenderer),
            ),
        )
        .expect("built-in number payload subscription must be valid");
    registry
        .register_collected_payload_subscription::<signal_processing::TextSample>(
            DefaultViewerPayloadPresentation::with_renderer(
                ViewerLaneBadge::new("TXT", Color32::from_rgb(215, 150, 170)),
                Arc::new(TextSnapshotRenderer),
            ),
        )
        .expect("built-in text payload subscription must be valid");
}
