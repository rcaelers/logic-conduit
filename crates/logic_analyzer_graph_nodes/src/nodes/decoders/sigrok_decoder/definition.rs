use std::collections::HashSet;
use std::path::PathBuf;

use egui::{Color32, Rect, Ui};
use serde::{Deserialize, Serialize};

use logic_analyzer_processing::support::{
    SigrokDecoderDescriptor, SigrokOutputKind, SigrokScalarValue,
};
use node_graph::{
    BoolValue, EnumValue, FloatValue, InlineControl, InputDef, IntValue, NodeBadge, NodeDef,
    NodeInstanceSchema, OutputDef, PanelSection, PropDef, Socket, SocketDef, SocketShape,
    StringValue,
};

use crate::nodes::registry::{COLOR_DECODERS, Signal};

const CURRENT_SCHEMA_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum SavedOutputKind {
    Annotation,
    Binary,
    GeneratedLogic,
    Metadata,
    ProtocolPacket,
}

impl SavedOutputKind {
    pub(crate) fn port_name(self) -> &'static str {
        match self {
            Self::Annotation => "annotations",
            Self::Binary => "binary",
            Self::GeneratedLogic => "logic",
            Self::Metadata => "metadata",
            Self::ProtocolPacket => "packets",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Annotation => "Annotations",
            Self::Binary => "Binary",
            Self::GeneratedLogic => "Generated Logic",
            Self::Metadata => "Metadata",
            Self::ProtocolPacket => "Packets",
        }
    }
}

impl From<SigrokOutputKind> for SavedOutputKind {
    fn from(value: SigrokOutputKind) -> Self {
        match value {
            SigrokOutputKind::Annotation => Self::Annotation,
            SigrokOutputKind::Binary => Self::Binary,
            SigrokOutputKind::GeneratedLogic => Self::GeneratedLogic,
            SigrokOutputKind::Metadata => Self::Metadata,
            SigrokOutputKind::ProtocolPacket => Self::ProtocolPacket,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SavedChannel {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) required: bool,
    pub(crate) enabled: BoolValue,
    pub(crate) initial_pin: EnumValue,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum SavedOptionControl {
    Bool(BoolValue),
    Integer(IntValue),
    Float(FloatValue),
    String(StringValue),
    Choice {
        selected: EnumValue,
        values: Vec<SavedScalar>,
    },
}

impl InlineControl for SavedOptionControl {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        match self {
            Self::Bool(value) => value.draw_widget(ui, label, rect, zoom, clip_rect),
            Self::Integer(value) => value.draw_widget(ui, label, rect, zoom, clip_rect),
            Self::Float(value) => value.draw_widget(ui, label, rect, zoom, clip_rect),
            Self::String(value) => value.draw_widget(ui, label, rect, zoom, clip_rect),
            Self::Choice { selected, .. } => selected.draw_widget(ui, label, rect, zoom, clip_rect),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum SavedScalar {
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

impl From<&SigrokScalarValue> for SavedScalar {
    fn from(value: &SigrokScalarValue) -> Self {
        match value {
            SigrokScalarValue::Bool(value) => Self::Bool(*value),
            SigrokScalarValue::Integer(value) => Self::Integer(*value),
            SigrokScalarValue::Float(value) => Self::Float(*value),
            SigrokScalarValue::String(value) => Self::String(value.clone()),
        }
    }
}

impl SavedScalar {
    fn label(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Integer(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            Self::String(value) => value.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SavedOption {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) control: SavedOptionControl,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SavedAnnotationRow {
    pub(crate) classes: Vec<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct SigrokDecoderState {
    #[serde(default)]
    pub(crate) schema_version: u8,
    #[serde(default)]
    pub(crate) decoder_root: PathBuf,
    #[serde(default)]
    pub(crate) decoder_id: String,
    #[serde(default)]
    pub(crate) decoder_name: String,
    #[serde(default)]
    pub(crate) package_fingerprint: String,
    #[serde(default)]
    pub(crate) sample_rate: Option<IntValue>,
    #[serde(default)]
    pub(crate) channels: Vec<SavedChannel>,
    #[serde(default)]
    pub(crate) options: Vec<SavedOption>,
    #[serde(default)]
    pub(crate) outputs: Vec<SavedOutputKind>,
    #[serde(default)]
    pub(crate) annotation_rows: Vec<SavedAnnotationRow>,
    #[serde(default)]
    pub(crate) annotation_class_count: usize,
    #[serde(default)]
    pub(crate) binary_class_count: usize,
    #[serde(default)]
    pub(crate) logic_groups: Vec<String>,
    #[serde(skip)]
    pub(crate) compatibility_warning: Option<String>,
}

impl SigrokDecoderState {
    pub(crate) fn from_descriptor(
        decoder_root: PathBuf,
        descriptor: &SigrokDecoderDescriptor,
    ) -> Self {
        let channels = descriptor
            .channels
            .iter()
            .map(|channel| SavedChannel {
                id: channel.id.clone(),
                label: channel.name.clone(),
                required: true,
                enabled: BoolValue::new(true),
                initial_pin: initial_pin_control(),
            })
            .chain(
                descriptor
                    .optional_channels
                    .iter()
                    .map(|channel| SavedChannel {
                        id: channel.id.clone(),
                        label: channel.name.clone(),
                        required: false,
                        enabled: BoolValue::new(false),
                        initial_pin: initial_pin_control(),
                    }),
            )
            .collect();
        let options = descriptor
            .options
            .iter()
            .map(|option| SavedOption {
                id: option.id.clone(),
                label: option.description.clone(),
                control: option_control(&option.default, &option.values),
            })
            .collect();
        let mut outputs = descriptor
            .registered_outputs
            .iter()
            .copied()
            .map(Into::into)
            .collect::<Vec<_>>();
        outputs.sort_by_key(|output| match output {
            SavedOutputKind::Annotation => 0,
            SavedOutputKind::Binary => 1,
            SavedOutputKind::GeneratedLogic => 2,
            SavedOutputKind::Metadata => 3,
            SavedOutputKind::ProtocolPacket => 4,
        });
        outputs.dedup();
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            decoder_root,
            decoder_id: descriptor.id.clone(),
            decoder_name: descriptor.name.clone(),
            package_fingerprint: descriptor.package_fingerprint.clone(),
            sample_rate: Some(IntValue::new(1_000_000, 1, i32::MAX)),
            channels,
            options,
            outputs,
            annotation_rows: descriptor
                .annotation_rows
                .iter()
                .map(|row| SavedAnnotationRow {
                    classes: row.classes.clone(),
                })
                .collect(),
            annotation_class_count: descriptor.annotations.len(),
            binary_class_count: descriptor.binary.len(),
            logic_groups: descriptor
                .logic_output_channels
                .iter()
                .map(|channel| channel.name.clone())
                .collect(),
            compatibility_warning: None,
        }
    }

    pub(crate) fn sample_rate(&self) -> Result<u64, String> {
        self.sample_rate
            .as_ref()
            .map(|value| value.value.max(1) as u64)
            .ok_or_else(|| "Sigrok decoder has no saved sample rate".to_owned())
    }
}

pub(crate) struct SigrokDecoderDefinition;

impl NodeDef for SigrokDecoderDefinition {
    type State = SigrokDecoderState;

    fn name() -> &'static str {
        "Sigrok Decoder"
    }

    fn category() -> &'static str {
        "Decoders"
    }

    fn color() -> Color32 {
        COLOR_DECODERS
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        Vec::new()
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        Vec::new()
    }

    fn state() -> Self::State {
        SigrokDecoderState::default()
    }

    fn instance_schema(state: &Self::State) -> NodeInstanceSchema<Self::State> {
        let inputs = state
            .channels
            .iter()
            .map(|channel| InputDef::new::<Signal>(&channel.label).stable_id(channel.id.clone()))
            .collect();
        let outputs = state
            .outputs
            .iter()
            .copied()
            .map(output_definition)
            .collect();
        let mut settings = Vec::new();
        if state.sample_rate.is_some() {
            settings.push(PropDef::instance_control(
                "sample_rate",
                "Sample rate (Hz)",
                |state: &mut SigrokDecoderState| {
                    state
                        .sample_rate
                        .as_mut()
                        .expect("schema checked sample rate")
                },
            ));
        }
        for (index, channel) in state.channels.iter().enumerate() {
            if !channel.required {
                settings.push(PropDef::instance_control(
                    format!("channel.{}.enabled", channel.id),
                    format!("Enable {}", channel.label),
                    move |state: &mut SigrokDecoderState| &mut state.channels[index].enabled,
                ));
            }
            settings.push(PropDef::instance_control(
                format!("channel.{}.initial", channel.id),
                format!("{} initial level", channel.label),
                move |state: &mut SigrokDecoderState| &mut state.channels[index].initial_pin,
            ));
        }
        let options = state
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                PropDef::instance_control(
                    format!("option.{}", option.id),
                    option.label.clone(),
                    move |state: &mut SigrokDecoderState| &mut state.options[index].control,
                )
            })
            .collect::<Vec<_>>();
        let mut panel = Vec::new();
        if !settings.is_empty() {
            panel.push(PanelSection::new("Channels", settings));
        }
        if !options.is_empty() {
            panel.push(PanelSection::new("Options", options));
        }
        NodeInstanceSchema::new(inputs, outputs).panel(panel)
    }

    fn on_update(state: &mut Self::State, inputs: &mut [Socket], _outputs: &mut [Socket]) {
        if state.schema_version == 0 && !state.decoder_id.is_empty() {
            state.schema_version = CURRENT_SCHEMA_VERSION;
            state.compatibility_warning = Some(
                "Upgraded the saved Sigrok decoder schema; socket identities were preserved"
                    .to_owned(),
            );
        }
        for input in inputs.iter_mut() {
            input.visible = state
                .channels
                .get(input.def_index)
                .is_some_and(|channel| channel.required || channel.enabled.value);
        }
    }

    fn badge(state: &Self::State) -> Option<NodeBadge> {
        if state.decoder_id.is_empty() {
            Some(NodeBadge::warning("No Sigrok decoder is selected"))
        } else if let Some(warning) = &state.compatibility_warning {
            Some(NodeBadge::warning(warning))
        } else {
            validate_saved_schema(state).err().map(NodeBadge::error)
        }
    }
}

fn initial_pin_control() -> EnumValue {
    EnumValue::new(2, &["Low", "High", "Same as first sample"])
}

fn option_control(default: &SigrokScalarValue, values: &[SigrokScalarValue]) -> SavedOptionControl {
    if !values.is_empty() {
        let values = values.iter().map(SavedScalar::from).collect::<Vec<_>>();
        let variants = values.iter().map(SavedScalar::label).collect::<Vec<_>>();
        let default = SavedScalar::from(default).label();
        let index = variants
            .iter()
            .position(|variant| variant == &default)
            .unwrap_or(0);
        return SavedOptionControl::Choice {
            selected: EnumValue { index, variants },
            values,
        };
    }
    match default {
        SigrokScalarValue::Bool(value) => SavedOptionControl::Bool(BoolValue::new(*value)),
        SigrokScalarValue::Integer(value) => SavedOptionControl::Integer(IntValue::plain(
            (*value).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        )),
        SigrokScalarValue::Float(value) => {
            SavedOptionControl::Float(FloatValue::plain(*value as f32))
        }
        SigrokScalarValue::String(value) => SavedOptionControl::String(StringValue::new(value)),
    }
}

fn validate_saved_schema(state: &SigrokDecoderState) -> Result<(), String> {
    let mut identities = HashSet::new();
    for channel in &state.channels {
        if channel.id.is_empty() || !identities.insert(("channel", channel.id.as_str())) {
            return Err("The saved Sigrok decoder has invalid channel identities".to_owned());
        }
    }
    for option in &state.options {
        if option.id.is_empty() || !identities.insert(("option", option.id.as_str())) {
            return Err("The saved Sigrok decoder has invalid option identities".to_owned());
        }
    }
    Ok(())
}

fn output_definition(output: SavedOutputKind) -> OutputDef<SigrokDecoderState> {
    let definition = match output {
        SavedOutputKind::Annotation => OutputDef::new::<SigrokAnnotationSocket>(output.label()),
        SavedOutputKind::Binary => OutputDef::new::<SigrokBinarySocket>(output.label()),
        SavedOutputKind::GeneratedLogic => {
            OutputDef::new::<SigrokGeneratedLogicSocket>(output.label())
        }
        SavedOutputKind::Metadata => OutputDef::new::<SigrokMetadataSocket>(output.label()),
        SavedOutputKind::ProtocolPacket => {
            OutputDef::new::<SigrokProtocolPacketSocket>(output.label())
        }
    };
    definition.stable_id(output.port_name())
}

macro_rules! socket_type {
    ($name:ident, $type_name:literal, $color:expr) => {
        struct $name;
        impl SocketDef for $name {
            type Value = ();

            fn type_name() -> &'static str {
                $type_name
            }

            fn color() -> Color32 {
                $color
            }

            fn shape() -> SocketShape {
                SocketShape::Diamond
            }
        }
    };
}

socket_type!(
    SigrokAnnotationSocket,
    "Sigrok Annotation",
    Color32::from_rgb(220, 155, 65)
);
socket_type!(
    SigrokBinarySocket,
    "Sigrok Binary",
    Color32::from_rgb(205, 125, 55)
);
socket_type!(
    SigrokGeneratedLogicSocket,
    "Sigrok Logic",
    Color32::from_rgb(95, 175, 95)
);
socket_type!(
    SigrokMetadataSocket,
    "Sigrok Metadata",
    Color32::from_rgb(95, 145, 210)
);
socket_type!(
    SigrokProtocolPacketSocket,
    "Sigrok Packet",
    Color32::from_rgb(175, 120, 205)
);

#[cfg(test)]
mod definition_tests {
    use super::*;

    #[test]
    fn instance_schema_uses_saved_stable_channels_options_and_outputs() {
        let state = fixture_state();
        let mut registry = node_graph::NodeTypeRegistry::new();
        registry.register::<SigrokDecoderDefinition>();
        let mut widget = node_graph::NodeGraphWidget::new(registry);
        let node = widget
            .add_node_at(SigrokDecoderDefinition::name(), egui::Pos2::ZERO)
            .unwrap();
        assert!(widget.set_node_state(node, serde_json::to_value(state).unwrap()));
        let node = &widget.graph().nodes[&node];
        assert_eq!(
            node.inputs
                .iter()
                .map(|input| input.name.as_str())
                .collect::<Vec<_>>(),
            ["Clock", "Data"]
        );
        assert!(node.inputs[0].visible);
        assert!(!node.inputs[1].visible);
        assert_eq!(
            node.outputs
                .iter()
                .map(|output| output.name.as_str())
                .collect::<Vec<_>>(),
            ["Annotations", "Packets"]
        );

        let saved = serde_json::to_string(widget.graph()).unwrap();
        let mut registry = node_graph::NodeTypeRegistry::new();
        registry.register::<SigrokDecoderDefinition>();
        let mut restored = node_graph::NodeGraphWidget::new(registry);
        restored.set_graph(serde_json::from_str(&saved).unwrap());
        let node = &restored.graph().nodes[&node.id];
        assert_eq!(node.inputs[0].name, "Clock");
        assert_eq!(node.outputs[1].name, "Packets");
    }

    #[test]
    fn legacy_saved_state_migrates_with_a_visible_warning() {
        let mut state = fixture_state();
        state.schema_version = 0;
        let mut registry = node_graph::NodeTypeRegistry::new();
        registry.register::<SigrokDecoderDefinition>();
        let mut widget = node_graph::NodeGraphWidget::new(registry);
        let node = widget
            .add_node_at(SigrokDecoderDefinition::name(), egui::Pos2::ZERO)
            .unwrap();
        assert!(widget.set_node_state(node, serde_json::to_value(state).unwrap()));
        let state: SigrokDecoderState =
            serde_json::from_value(widget.graph().nodes[&node].state.clone()).unwrap();
        assert_eq!(state.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(
            widget.graph().nodes[&node]
                .badge
                .as_ref()
                .is_some_and(|badge| badge.text.contains("Upgraded"))
        );
    }

    fn fixture_state() -> SigrokDecoderState {
        SigrokDecoderState {
            schema_version: CURRENT_SCHEMA_VERSION,
            decoder_root: PathBuf::from("/decoders"),
            decoder_id: "fixture".into(),
            decoder_name: "Fixture".into(),
            package_fingerprint: "abc".into(),
            sample_rate: Some(IntValue::new(1_000_000, 1, i32::MAX)),
            channels: vec![
                SavedChannel {
                    id: "clk".into(),
                    label: "Clock".into(),
                    required: true,
                    enabled: BoolValue::new(true),
                    initial_pin: initial_pin_control(),
                },
                SavedChannel {
                    id: "data".into(),
                    label: "Data".into(),
                    required: false,
                    enabled: BoolValue::new(false),
                    initial_pin: initial_pin_control(),
                },
            ],
            options: vec![SavedOption {
                id: "mode".into(),
                label: "Mode".into(),
                control: SavedOptionControl::Choice {
                    selected: EnumValue::new(0, &["A", "B"]),
                    values: vec![
                        SavedScalar::String("A".into()),
                        SavedScalar::String("B".into()),
                    ],
                },
            }],
            outputs: vec![SavedOutputKind::Annotation, SavedOutputKind::ProtocolPacket],
            annotation_rows: vec![SavedAnnotationRow { classes: vec![0] }],
            annotation_class_count: 1,
            binary_class_count: 0,
            logic_groups: Vec::new(),
            compatibility_warning: None,
        }
    }
}
