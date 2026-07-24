use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use logic_analyzer_graph_api::node::RuntimeBuilder;
use logic_analyzer_graph_api::node_support::{
    NodeBuildContext, PortKind, ResolvedInputs, parse_state,
};
use logic_analyzer_processing::nodes::decoders::sigrok_decoder::{
    SigrokAnnotation, SigrokBinary, SigrokChannel, SigrokDecoder, SigrokDecoderConfig,
    SigrokGeneratedLogic, SigrokInitialPin, SigrokMetadata, SigrokOptionValue,
    SigrokProtocolPacket,
};
use logic_analyzer_processing::support::{SigrokDecoderDescriptor, discover_sigrok_decoder};
use node_graph::Socket;
use signal_processing::{ProcessNode, SampleBlock};

use super::definition::{SavedOptionControl, SavedOutputKind, SavedScalar, SigrokDecoderState};

#[derive(Default)]
pub(crate) struct SigrokDecoderBuilder;

impl SigrokDecoderBuilder {
    fn parsed(state: &Value) -> Result<SigrokDecoderState, String> {
        parse_state(state)
    }
}

impl RuntimeBuilder for SigrokDecoderBuilder {
    fn accepted_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
        let Ok(state) = Self::parsed(state) else {
            return Vec::new();
        };
        if socket.def_index == state.channels.len() && !state.protocol_inputs.is_empty() {
            vec![PortKind::of_named::<SigrokProtocolPacket>("Sigrok Packet")]
        } else {
            vec![PortKind::of::<SampleBlock>()]
        }
    }

    fn offered_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
        let Ok(state) = Self::parsed(state) else {
            return Vec::new();
        };
        state
            .outputs
            .get(socket.def_index)
            .copied()
            .map(output_kind)
            .into_iter()
            .collect()
    }

    fn offered_connection_contracts(&self, socket: &Socket, state: &Value) -> Vec<String> {
        let Ok(state) = Self::parsed(state) else {
            return Vec::new();
        };
        if state
            .outputs
            .get(socket.def_index)
            .is_some_and(|output| *output == SavedOutputKind::ProtocolPacket)
        {
            state.protocol_outputs
        } else {
            Vec::new()
        }
    }

    fn accepted_connection_contracts(&self, socket: &Socket, state: &Value) -> Vec<String> {
        let Ok(state) = Self::parsed(state) else {
            return Vec::new();
        };
        if socket.def_index == state.channels.len() && !state.protocol_inputs.is_empty() {
            state.protocol_inputs
        } else {
            Vec::new()
        }
    }

    fn input_port(
        &self,
        socket: &Socket,
        _member_index: usize,
        state: &Value,
        kind: PortKind,
    ) -> Option<String> {
        let state = Self::parsed(state).ok()?;
        if socket.def_index == state.channels.len() && !state.protocol_inputs.is_empty() {
            return (kind == PortKind::of_named::<SigrokProtocolPacket>("Sigrok Packet"))
                .then(|| "packets".to_owned());
        }
        if kind != PortKind::of::<SampleBlock>() {
            return None;
        }
        state
            .channels
            .get(socket.def_index)
            .map(|channel| channel.id.clone())
    }

    fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String> {
        let state = Self::parsed(state).ok()?;
        let output = *state.outputs.get(socket.def_index)?;
        (kind == output_kind(output)).then(|| output.port_name().to_owned())
    }

    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        let Ok(state) = Self::parsed(state) else {
            return true;
        };
        if socket.def_index == state.channels.len() && !state.protocol_inputs.is_empty() {
            return true;
        }
        state
            .channels
            .get(socket.def_index)
            .is_none_or(|channel| channel.required || channel.enabled.value)
    }

    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut dyn NodeBuildContext,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state = Self::parsed(state)?;
        if state.decoder_id.is_empty() {
            return Err("No Sigrok decoder is selected".to_owned());
        }
        let current = discover_sigrok_decoder(&state.decoder_root, &state.decoder_id)?;
        if current.package_fingerprint != state.package_fingerprint {
            return Err(format!(
                "Sigrok decoder '{}' changed since this graph was saved; reselect it to migrate its channels and options",
                state.decoder_id
            ));
        }
        validate_descriptor_schema(&state, &current)?;
        let channels = state
            .channels
            .iter()
            .map(|channel| SigrokChannel {
                name: channel.id.clone(),
                connected: channel.required || channel.enabled.value,
                initial_pin: match channel.initial_pin.selected() {
                    "Low" => SigrokInitialPin::Low,
                    "High" => SigrokInitialPin::High,
                    _ => SigrokInitialPin::SameAsFirstSample,
                },
            })
            .collect();
        let options = state
            .options
            .iter()
            .map(|option| Ok((option.id.clone(), option_value(&option.control)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let mut annotation_rows_by_class = vec![Vec::new(); state.annotation_class_count];
        for (row, descriptor) in state.annotation_rows.iter().enumerate() {
            for &class in &descriptor.classes {
                let Some(rows) = annotation_rows_by_class.get_mut(class) else {
                    return Err(format!(
                        "Sigrok decoder '{}' has an invalid saved annotation class {class}",
                        state.decoder_id
                    ));
                };
                rows.push(row);
            }
        }
        let sample_rate = state.sample_rate()?;
        let decoder = SigrokDecoder::new(SigrokDecoderConfig {
            decoder_root: state.decoder_root,
            decoder_id: state.decoder_id,
            sample_rate,
            channels,
            protocol_inputs: state.protocol_inputs,
            options,
            annotation_rows_by_class: annotation_rows_by_class
                .into_iter()
                .map(Arc::from)
                .collect(),
            binary_class_count: state.binary_class_count,
            logic_groups: state.logic_groups,
        })?
        .with_name(name);
        Ok(Box::new(decoder))
    }
}

fn output_kind(output: SavedOutputKind) -> PortKind {
    match output {
        SavedOutputKind::Annotation => PortKind::of_named::<SigrokAnnotation>("Sigrok Annotation"),
        SavedOutputKind::Binary => PortKind::of_named::<SigrokBinary>("Sigrok Binary"),
        SavedOutputKind::GeneratedLogic => {
            PortKind::of_named::<SigrokGeneratedLogic>("Sigrok Logic")
        }
        SavedOutputKind::Metadata => PortKind::of_named::<SigrokMetadata>("Sigrok Metadata"),
        SavedOutputKind::ProtocolPacket => {
            PortKind::of_named::<SigrokProtocolPacket>("Sigrok Packet")
        }
    }
}

fn option_value(control: &SavedOptionControl) -> Result<SigrokOptionValue, String> {
    match control {
        SavedOptionControl::Bool(value) => Ok(SigrokOptionValue::Bool(value.value)),
        SavedOptionControl::Integer(value) => {
            Ok(SigrokOptionValue::Integer(i64::from(value.value)))
        }
        SavedOptionControl::Float(value) => Ok(SigrokOptionValue::Float(f64::from(value.value))),
        SavedOptionControl::String(value) => Ok(SigrokOptionValue::String(value.value.clone())),
        SavedOptionControl::Choice { selected, values } => values
            .get(selected.index)
            .ok_or_else(|| "Sigrok decoder option selection is invalid".to_owned())
            .map(scalar_value),
    }
}

fn scalar_value(value: &SavedScalar) -> SigrokOptionValue {
    match value {
        SavedScalar::Bool(value) => SigrokOptionValue::Bool(*value),
        SavedScalar::Integer(value) => SigrokOptionValue::Integer(*value),
        SavedScalar::Float(value) => SigrokOptionValue::Float(*value),
        SavedScalar::String(value) => SigrokOptionValue::String(value.clone()),
    }
}

fn validate_descriptor_schema(
    state: &SigrokDecoderState,
    descriptor: &SigrokDecoderDescriptor,
) -> Result<(), String> {
    let expected = SigrokDecoderState::from_descriptor(state.decoder_root.clone(), descriptor);
    let current_channels = expected
        .channels
        .iter()
        .map(|channel| (channel.id.as_str(), channel.required))
        .collect::<Vec<_>>();
    let saved_channels = state
        .channels
        .iter()
        .map(|channel| (channel.id.as_str(), channel.required))
        .collect::<Vec<_>>();
    if current_channels != saved_channels {
        return Err(format!(
            "Sigrok decoder '{}' channel schema changed; reselect it to migrate the graph",
            state.decoder_id
        ));
    }
    let current_options = expected
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<Vec<_>>();
    let saved_options = state
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<Vec<_>>();
    if current_options != saved_options {
        return Err(format!(
            "Sigrok decoder '{}' option schema changed; reselect it to migrate the graph",
            state.decoder_id
        ));
    }
    if expected.outputs != state.outputs
        || expected.protocol_inputs != state.protocol_inputs
        || expected.protocol_outputs != state.protocol_outputs
        || expected.annotation_class_count != state.annotation_class_count
        || expected.binary_class_count != state.binary_class_count
        || expected.logic_groups != state.logic_groups
    {
        return Err(format!(
            "Sigrok decoder '{}' output schema changed; reselect it to migrate the graph",
            state.decoder_id
        ));
    }
    Ok(())
}
