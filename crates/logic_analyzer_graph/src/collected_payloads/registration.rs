//! Built-in collected-payload inventory submissions.

use std::sync::Arc;

use egui::Color32;

use logic_analyzer_viewer::{DefaultViewerLaneRenderer, ViewerLaneBadge};
use signal_processing::{CollectedLaneRequest, CollectedWordLaneOptions, LiveStoreConfig};

use super::presentation::{
    DigitalSnapshotRenderer, NumberSnapshotRenderer, TextSnapshotRenderer, TriggerSnapshotRenderer,
    WordSnapshotRenderer,
};
use crate::{
    CollectedPayloadRegistration, DefaultViewerPayloadPresentation, NodeBuildContext, ResolvedInput,
};

fn digital_presentation() -> DefaultViewerPayloadPresentation {
    DefaultViewerPayloadPresentation::with_renderer(
        ViewerLaneBadge::new("S", Color32::from_rgb(95, 175, 95)),
        Arc::new(DigitalSnapshotRenderer),
    )
}

fn word_presentation() -> DefaultViewerPayloadPresentation {
    DefaultViewerPayloadPresentation::with_renderer(
        ViewerLaneBadge::new("W", Color32::from_rgb(215, 140, 60)),
        Arc::new(WordSnapshotRenderer::new(Arc::new(
            DefaultViewerLaneRenderer,
        ))),
    )
}

fn word_request(
    request: CollectedLaneRequest,
    member: usize,
    input: &ResolvedInput,
    ctx: &dyn NodeBuildContext,
) -> CollectedLaneRequest {
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
}

fn trigger_presentation() -> DefaultViewerPayloadPresentation {
    DefaultViewerPayloadPresentation::with_renderer(
        ViewerLaneBadge::new("T", Color32::from_rgb(230, 190, 80)),
        Arc::new(TriggerSnapshotRenderer),
    )
}

fn number_presentation() -> DefaultViewerPayloadPresentation {
    DefaultViewerPayloadPresentation::with_renderer(
        ViewerLaneBadge::new("N", Color32::from_rgb(95, 145, 210)),
        Arc::new(NumberSnapshotRenderer),
    )
}

fn text_presentation() -> DefaultViewerPayloadPresentation {
    DefaultViewerPayloadPresentation::with_renderer(
        ViewerLaneBadge::new("TXT", Color32::from_rgb(215, 150, 170)),
        Arc::new(TextSnapshotRenderer),
    )
}

inventory::submit! {
    CollectedPayloadRegistration::subscribable::<signal_processing::Sample>(
        "org.logicconduit.digital-sample/v1",
        signal_processing::digital_payload_adapter,
        digital_presentation,
    )
}

inventory::submit! {
    CollectedPayloadRegistration::subscribable_with_request_configurator::<signal_processing::Word>(
        "org.logicconduit.word/v1",
        signal_processing::word_payload_adapter,
        word_presentation,
        word_request,
        true,
    )
}

inventory::submit! {
    CollectedPayloadRegistration::subscribable::<signal_processing::Trigger>(
        "org.logicconduit.trigger/v1",
        signal_processing::trigger_payload_adapter,
        trigger_presentation,
    )
}

inventory::submit! {
    CollectedPayloadRegistration::subscribable::<signal_processing::NumberSample>(
        "org.logicconduit.number-sample/v1",
        signal_processing::number_payload_adapter,
        number_presentation,
    )
}

inventory::submit! {
    CollectedPayloadRegistration::subscribable::<signal_processing::TextSample>(
        "org.logicconduit.text-sample/v1",
        signal_processing::text_payload_adapter,
        text_presentation,
    )
}
