use logic_analyzer_processing::nodes::decoders::sigrok_decoder::{
    SigrokAnnotation, SigrokBinary, SigrokGeneratedLogic, SigrokLaneSnapshot, SigrokMetadata,
    SigrokProtocolPacket,
};
use logic_analyzer_viewer::{
    OpaqueLaneDrawContext, ViewerLaneRenderer, ViewerLaneTrack, draw_digital_snapshot,
    draw_value_snapshot,
};
use signal_processing::{OpaqueCollectedLaneSnapshot, Sample};

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
span_renderer!(SigrokMetadataRenderer, SigrokMetadata);
span_renderer!(SigrokProtocolPacketRenderer, SigrokProtocolPacket);

pub(crate) struct SigrokGeneratedLogicRenderer;

impl ViewerLaneRenderer for SigrokGeneratedLogicRenderer {
    fn draw_opaque_lane(
        &self,
        _track: &ViewerLaneTrack,
        snapshot: Option<&OpaqueCollectedLaneSnapshot>,
        context: OpaqueLaneDrawContext<'_>,
    ) -> bool {
        let Some(snapshot) = snapshot
            .and_then(|snapshot| snapshot.value::<SigrokLaneSnapshot<SigrokGeneratedLogic>>())
        else {
            return false;
        };
        let mut transitions = Vec::new();
        let mut previous = None;
        for output in snapshot.entries() {
            let count = output.sample_count.min(output.samples.len());
            if count == 0 {
                continue;
            }
            let duration = output.end_time_ns.saturating_sub(output.start_time_ns);
            for (index, value) in output.samples.iter().copied().take(count).enumerate() {
                let value = value != 0;
                if previous == Some(value) {
                    continue;
                }
                let offset = (u128::from(duration) * index as u128 / count as u128) as u64;
                transitions.push(Sample::new(
                    value,
                    output.start_time_ns.saturating_add(offset),
                ));
                previous = Some(value);
            }
        }
        draw_digital_snapshot(&context, &transitions, false);
        true
    }
}
