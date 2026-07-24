use std::sync::Arc;

use egui::Color32;

use logic_analyzer_graph_api::node::CollectedPayloadRegistration;
use logic_analyzer_graph_api::node_support::{DefaultViewerPayloadPresentation, PortKind};
use logic_analyzer_processing::nodes::decoders::sigrok_decoder::{
    SigrokAnnotation, SigrokBinary, SigrokGeneratedLogic, SigrokMetadata, SigrokProtocolPacket,
    sigrok_annotation_payload_adapter, sigrok_binary_payload_adapter,
    sigrok_generated_logic_payload_adapter, sigrok_metadata_payload_adapter,
    sigrok_protocol_packet_payload_adapter,
};
use logic_analyzer_viewer::ViewerLaneBadge;

use super::presentation::{
    SigrokAnnotationRenderer, SigrokBinaryRenderer, SigrokGeneratedLogicRenderer,
    SigrokMetadataRenderer, SigrokProtocolPacketRenderer,
};

fn annotation_kind() -> PortKind {
    PortKind::of_named::<SigrokAnnotation>("Sigrok Annotation")
}

fn binary_kind() -> PortKind {
    PortKind::of_named::<SigrokBinary>("Sigrok Binary")
}

fn generated_logic_kind() -> PortKind {
    PortKind::of_named::<SigrokGeneratedLogic>("Sigrok Logic")
}

fn metadata_kind() -> PortKind {
    PortKind::of_named::<SigrokMetadata>("Sigrok Metadata")
}

fn protocol_packet_kind() -> PortKind {
    PortKind::of_named::<SigrokProtocolPacket>("Sigrok Packet")
}

fn annotation_presentation() -> DefaultViewerPayloadPresentation {
    DefaultViewerPayloadPresentation::with_renderer(
        ViewerLaneBadge::new("A", Color32::from_rgb(220, 155, 65)),
        Arc::new(SigrokAnnotationRenderer),
    )
}

fn binary_presentation() -> DefaultViewerPayloadPresentation {
    DefaultViewerPayloadPresentation::with_renderer(
        ViewerLaneBadge::new("BIN", Color32::from_rgb(205, 125, 55)),
        Arc::new(SigrokBinaryRenderer),
    )
}

fn generated_logic_presentation() -> DefaultViewerPayloadPresentation {
    DefaultViewerPayloadPresentation::with_renderer(
        ViewerLaneBadge::new("S", Color32::from_rgb(95, 175, 95)),
        Arc::new(SigrokGeneratedLogicRenderer),
    )
}

fn metadata_presentation() -> DefaultViewerPayloadPresentation {
    DefaultViewerPayloadPresentation::with_renderer(
        ViewerLaneBadge::new("M", Color32::from_rgb(95, 145, 210)),
        Arc::new(SigrokMetadataRenderer),
    )
}

fn protocol_packet_presentation() -> DefaultViewerPayloadPresentation {
    DefaultViewerPayloadPresentation::with_renderer(
        ViewerLaneBadge::new("P", Color32::from_rgb(175, 120, 205)),
        Arc::new(SigrokProtocolPacketRenderer),
    )
}

inventory::submit! {
    CollectedPayloadRegistration::subscribable_kind(
        "org.logicconduit.sigrok.annotation/v1",
        annotation_kind,
        sigrok_annotation_payload_adapter,
        annotation_presentation,
    )
}

#[cfg(test)]
mod registration_tests {
    use super::*;

    #[test]
    fn sigrok_payload_kinds_have_distinct_open_type_identities() {
        let kinds = [
            annotation_kind(),
            binary_kind(),
            generated_logic_kind(),
            metadata_kind(),
            protocol_packet_kind(),
        ];
        for (index, kind) in kinds.iter().enumerate() {
            assert!(kinds.iter().skip(index + 1).all(|other| kind != other));
        }
    }

    #[test]
    fn all_sigrok_payload_contracts_are_submitted_to_inventory() {
        let stable_ids = inventory::iter::<CollectedPayloadRegistration>
            .into_iter()
            .map(CollectedPayloadRegistration::stable_id)
            .collect::<std::collections::HashSet<_>>();
        for stable_id in [
            "org.logicconduit.sigrok.annotation/v1",
            "org.logicconduit.sigrok.binary/v1",
            "org.logicconduit.sigrok.generated-logic/v1",
            "org.logicconduit.sigrok.metadata/v1",
            "org.logicconduit.sigrok.protocol-packet/v1",
        ] {
            assert!(stable_ids.contains(stable_id));
        }
    }
}

inventory::submit! {
    CollectedPayloadRegistration::subscribable_kind(
        "org.logicconduit.sigrok.binary/v1",
        binary_kind,
        sigrok_binary_payload_adapter,
        binary_presentation,
    )
}

inventory::submit! {
    CollectedPayloadRegistration::subscribable_kind(
        "org.logicconduit.sigrok.generated-logic/v1",
        generated_logic_kind,
        sigrok_generated_logic_payload_adapter,
        generated_logic_presentation,
    )
}

inventory::submit! {
    CollectedPayloadRegistration::subscribable_kind(
        "org.logicconduit.sigrok.metadata/v1",
        metadata_kind,
        sigrok_metadata_payload_adapter,
        metadata_presentation,
    )
}

inventory::submit! {
    CollectedPayloadRegistration::subscribable_kind(
        "org.logicconduit.sigrok.protocol-packet/v1",
        protocol_packet_kind,
        sigrok_protocol_packet_payload_adapter,
        protocol_packet_presentation,
    )
}
