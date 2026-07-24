use logic_analyzer_processing::nodes::decoders::sigrok_decoder::{
    SigrokAnnotation, SigrokBinary, SigrokGeneratedLogic, SigrokLaneSnapshot, SigrokMetadata,
    SigrokProtocolPacket,
};
use logic_analyzer_viewer::{
    OpaqueLaneDrawContext, ViewerLaneRenderer, ViewerLaneTrack, draw_value_snapshot,
};
use signal_processing::OpaqueCollectedLaneSnapshot;

macro_rules! span_renderer {
    ($renderer:ident, $payload:ty) => {
        pub(crate) struct $renderer;

        impl ViewerLaneRenderer for $renderer {
            fn draw_opaque_lane(
                &self,
                _track: &ViewerLaneTrack,
                snapshot: Option<&OpaqueCollectedLaneSnapshot>,
                context: OpaqueLaneDrawContext<'_>,
            ) -> bool {
                let Some(snapshot) =
                    snapshot.and_then(|snapshot| snapshot.value::<SigrokLaneSnapshot<$payload>>())
                else {
                    return false;
                };
                let values = snapshot
                    .entries()
                    .iter()
                    .map(|entry| (entry.start_time_ns, entry.display_text()))
                    .collect::<Vec<_>>();
                draw_value_snapshot(&context, &values, context.theme.accent);
                true
            }
        }
    };
}

span_renderer!(SigrokAnnotationRenderer, SigrokAnnotation);
span_renderer!(SigrokBinaryRenderer, SigrokBinary);
span_renderer!(SigrokGeneratedLogicRenderer, SigrokGeneratedLogic);
span_renderer!(SigrokMetadataRenderer, SigrokMetadata);
span_renderer!(SigrokProtocolPacketRenderer, SigrokProtocolPacket);
