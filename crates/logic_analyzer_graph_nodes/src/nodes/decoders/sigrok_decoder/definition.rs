use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;

use egui::{Color32, Rect, Ui};
use serde::{Deserialize, Serialize};

use logic_analyzer_processing::support::{
    SigrokCatalogEntry, SigrokDecoderCatalog, SigrokDecoderDescriptor, SigrokOutputKind,
    SigrokScalarValue,
};
use node_graph::{
    BoolValue, EnumValue, FloatValue, InlineControl, InputDef, IntValue, NodeBadge, NodeDef,
    NodeInstanceSchema, OutputDef, PanelSection, PropDef, Socket, SocketDef, SocketShape,
    StringValue,
};

use crate::nodes::registry::{COLOR_DECODERS, Signal};

const CURRENT_SCHEMA_VERSION: u8 = 2;

#[derive(Clone, Debug)]
pub(crate) struct CatalogChoice {
    pub(crate) decoder_root: PathBuf,
    pub(crate) decoder_id: String,
    pub(crate) label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SigrokCatalogControl {
    pub(crate) search_paths: String,
    pub(crate) selected_id: String,
    #[serde(skip)]
    pub(crate) entries: Vec<CatalogChoice>,
    #[serde(skip)]
    pub(crate) diagnostics: Vec<String>,
    #[serde(skip)]
    pub(crate) selection_diagnostic: Option<String>,
    #[serde(skip)]
    pub(crate) refresh_requested: bool,
}

impl Default for SigrokCatalogControl {
    fn default() -> Self {
        Self {
            search_paths: default_decoder_search_paths()
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            selected_id: String::new(),
            entries: Vec::new(),
            diagnostics: Vec::new(),
            selection_diagnostic: None,
            refresh_requested: false,
        }
    }
}

impl InlineControl for SigrokCatalogControl {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        _zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        let previous_paths = self.search_paths.clone();
        let previous_selection = self.selected_id.clone();
        let mut refresh = false;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.label(label);
                if self.entries.is_empty() {
                    ui.label("Click Refresh to scan the configured decoder directories.");
                }
                ui.horizontal(|ui| {
                    ui.label("Decoder:");
                    let selected = self
                        .entries
                        .iter()
                        .find(|entry| entry.decoder_id == self.selected_id)
                        .map_or("Select decoder", |entry| entry.label.as_str());
                    egui::ComboBox::from_id_salt("sigrok-decoder-catalog")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for entry in &self.entries {
                                ui.selectable_value(
                                    &mut self.selected_id,
                                    entry.decoder_id.clone(),
                                    &entry.label,
                                );
                            }
                        });
                    refresh = ui.button("Refresh").clicked();
                });
                ui.label("Search paths (one per line):");
                ui.add(
                    egui::TextEdit::multiline(&mut self.search_paths)
                        .desired_rows(2)
                        .desired_width(rect.width() - 8.0),
                );
                ui.colored_label(
                    Color32::from_rgb(225, 175, 80),
                    "Python decoders are trusted code and run with application permissions.",
                );
                if let Some(diagnostic) = &self.selection_diagnostic {
                    ui.colored_label(Color32::from_rgb(220, 100, 90), diagnostic);
                }
                for diagnostic in self.diagnostics.iter().take(2) {
                    ui.colored_label(Color32::from_rgb(220, 100, 90), diagnostic);
                }
            },
        );
        if refresh {
            self.refresh_requested = true;
        }
        refresh || self.search_paths != previous_paths || self.selected_id != previous_selection
    }
}

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

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub(crate) protocol_inputs: Vec<String>,
    #[serde(default)]
    pub(crate) protocol_outputs: Vec<String>,
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
    #[serde(default)]
    pub(crate) catalog: SigrokCatalogControl,
    #[serde(skip)]
    pub(crate) compatibility_warning: Option<String>,
}

impl Default for SigrokDecoderState {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            decoder_root: PathBuf::new(),
            decoder_id: String::new(),
            decoder_name: String::new(),
            package_fingerprint: String::new(),
            sample_rate: None,
            channels: Vec::new(),
            protocol_inputs: Vec::new(),
            protocol_outputs: Vec::new(),
            options: Vec::new(),
            outputs: Vec::new(),
            annotation_rows: Vec::new(),
            annotation_class_count: 0,
            binary_class_count: 0,
            logic_groups: Vec::new(),
            catalog: SigrokCatalogControl::default(),
            compatibility_warning: None,
        }
    }
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
            protocol_inputs: descriptor
                .inputs
                .iter()
                .filter(|input| input.as_str() != "logic")
                .cloned()
                .collect(),
            protocol_outputs: descriptor.outputs.clone(),
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
            catalog: SigrokCatalogControl {
                selected_id: descriptor.id.clone(),
                ..SigrokCatalogControl::default()
            },
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
            .chain((!state.protocol_inputs.is_empty()).then(|| {
                InputDef::new::<SigrokProtocolPacketSocket>("Packets").stable_id("protocol_packets")
            }))
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
        let mut panel = vec![PanelSection::new(
            "Decoder catalog",
            vec![
                PropDef::instance_control(
                    "catalog",
                    "Sigrok decoder catalog",
                    |state: &mut SigrokDecoderState| &mut state.catalog,
                )
                .panel_height(150.0),
            ],
        )];
        if !settings.is_empty() {
            panel.push(PanelSection::new("Channels", settings));
        }
        if !options.is_empty() {
            panel.push(PanelSection::new("Options", options));
        }
        NodeInstanceSchema::new(inputs, outputs).panel(panel)
    }

    fn on_update(state: &mut Self::State, inputs: &mut [Socket], _outputs: &mut [Socket]) {
        refresh_catalog(state);
        apply_catalog_selection(state);
        if state.schema_version < CURRENT_SCHEMA_VERSION
            && (!state.protocol_inputs.is_empty()
                || !state.protocol_outputs.is_empty()
                || !state.outputs.contains(&SavedOutputKind::ProtocolPacket))
        {
            state.schema_version = CURRENT_SCHEMA_VERSION;
            state.compatibility_warning = Some(
                "Upgraded the saved Sigrok decoder with explicit protocol connection contracts; existing socket identities were preserved"
                    .to_owned(),
            );
        }
        for input in inputs.iter_mut() {
            input.visible =
                if input.def_index == state.channels.len() && !state.protocol_inputs.is_empty() {
                    true
                } else {
                    state
                        .channels
                        .get(input.def_index)
                        .is_some_and(|channel| channel.required || channel.enabled.value)
                };
        }
    }

    fn badge(state: &Self::State) -> Option<NodeBadge> {
        if state.decoder_id.is_empty() {
            Some(NodeBadge::warning("No Sigrok decoder is selected"))
        } else if let Some(warning) = &state.compatibility_warning {
            Some(NodeBadge::warning(warning))
        } else if let Some(diagnostic) = &state.catalog.selection_diagnostic {
            Some(NodeBadge::warning(diagnostic))
        } else {
            validate_saved_schema(state).err().map(NodeBadge::error)
        }
    }
}

fn catalog() -> &'static SigrokDecoderCatalog {
    static CATALOG: OnceLock<SigrokDecoderCatalog> = OnceLock::new();
    CATALOG.get_or_init(SigrokDecoderCatalog::default)
}

fn catalog_search_paths(control: &SigrokCatalogControl) -> Vec<PathBuf> {
    control
        .search_paths
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn refresh_catalog(state: &mut SigrokDecoderState) {
    if !state.catalog.refresh_requested
        && (state.decoder_id.is_empty() || !state.catalog.entries.is_empty())
    {
        return;
    }
    let search_paths = catalog_search_paths(&state.catalog);
    let snapshot = if state.catalog.refresh_requested {
        catalog().refresh(&search_paths)
    } else {
        catalog().snapshot(&search_paths)
    };
    state.catalog.entries = snapshot.entries.iter().map(catalog_choice).collect();
    state.catalog.diagnostics = snapshot
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect();
    state.catalog.selection_diagnostic = None;
    if state.catalog.selected_id.is_empty() && !state.decoder_id.is_empty() {
        state.catalog.selected_id = state.decoder_id.clone();
    }
    if state.schema_version < CURRENT_SCHEMA_VERSION
        && let Some(current) = snapshot.entries.iter().find(|entry| {
            entry.descriptor.id == state.decoder_id
                && entry.descriptor.package_fingerprint == state.package_fingerprint
        })
    {
        state.protocol_inputs = current
            .descriptor
            .inputs
            .iter()
            .filter(|input| input.as_str() != "logic")
            .cloned()
            .collect();
        state.protocol_outputs = current.descriptor.outputs.clone();
        state.schema_version = CURRENT_SCHEMA_VERSION;
        state.compatibility_warning = Some(
            "Upgraded the saved Sigrok decoder with explicit protocol connection contracts; existing socket identities were preserved"
                .to_owned(),
        );
    }
    if let Some(current) = snapshot
        .entries
        .iter()
        .find(|entry| entry.descriptor.id == state.decoder_id)
        && current.descriptor.package_fingerprint != state.package_fingerprint
    {
        state.catalog.selection_diagnostic = Some(format!(
            "Sigrok decoder '{}' changed; reselect it to migrate its saved schema",
            state.decoder_id
        ));
    }
    if !state.decoder_id.is_empty()
        && !state
            .catalog
            .entries
            .iter()
            .any(|entry| entry.decoder_id == state.decoder_id)
    {
        state.catalog.selection_diagnostic = Some(format!(
            "Saved Sigrok decoder '{}' is unavailable; check its search path or Python dependencies",
            state.decoder_id
        ));
    }
    state.catalog.refresh_requested = false;
}

fn catalog_choice(entry: &SigrokCatalogEntry) -> CatalogChoice {
    CatalogChoice {
        decoder_root: entry.decoder_root.clone(),
        decoder_id: entry.descriptor.id.clone(),
        label: format!(
            "{} ({}, {})",
            entry.descriptor.name, entry.descriptor.id, entry.descriptor.license
        ),
    }
}

fn apply_catalog_selection(state: &mut SigrokDecoderState) {
    if state.catalog.selected_id.is_empty() || state.catalog.selected_id == state.decoder_id {
        return;
    }
    let selected = state
        .catalog
        .entries
        .iter()
        .find(|entry| entry.decoder_id == state.catalog.selected_id)
        .cloned();
    let Some(selected) = selected else {
        return;
    };
    let Some(entry) = catalog()
        .snapshot(&catalog_search_paths(&state.catalog))
        .entries
        .iter()
        .find(|entry| {
            entry.decoder_root == selected.decoder_root
                && entry.descriptor.id == selected.decoder_id
        })
        .cloned()
    else {
        return;
    };
    let catalog_control = state.catalog.clone();
    let mut selected_state =
        SigrokDecoderState::from_descriptor(entry.decoder_root, &entry.descriptor);
    selected_state.catalog = catalog_control;
    selected_state.catalog.selected_id = entry.descriptor.id;
    *state = selected_state;
}

fn default_decoder_search_paths() -> Vec<PathBuf> {
    let mut paths = std::env::var_os("SIGROK_DECODERS_DIR")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    for path in [
        PathBuf::from("/opt/homebrew/share/libsigrokdecode/decoders"),
        PathBuf::from("/usr/local/share/libsigrokdecode/decoders"),
        PathBuf::from("/usr/share/libsigrokdecode/decoders"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../dslogic/libsigrokdecode/decoders"),
    ] {
        if path.is_dir() && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
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
    if !state.channels.is_empty() && !state.protocol_inputs.is_empty() {
        return Err(
            "The saved Sigrok decoder mixes raw-logic and protocol input contracts".to_owned(),
        );
    }
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
    for protocol in &state.protocol_inputs {
        if protocol.is_empty() || !identities.insert(("protocol input", protocol.as_str())) {
            return Err(
                "The saved Sigrok decoder has invalid protocol input identities".to_owned(),
            );
        }
    }
    for protocol in &state.protocol_outputs {
        if protocol.is_empty() || !identities.insert(("protocol output", protocol.as_str())) {
            return Err(
                "The saved Sigrok decoder has invalid protocol output identities".to_owned(),
            );
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
            protocol_inputs: Vec::new(),
            protocol_outputs: vec!["fixture".into()],
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
            catalog: SigrokCatalogControl {
                refresh_requested: false,
                ..SigrokCatalogControl::default()
            },
            compatibility_warning: None,
        }
    }
}
