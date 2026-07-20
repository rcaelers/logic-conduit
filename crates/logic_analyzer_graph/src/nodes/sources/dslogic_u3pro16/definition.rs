//! `DSLogic U3Pro16` graph-node definition — native USB hardware capture source.

use egui::Color32;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use node_graph::{
    BoolValue, EnumValue, FloatValue, InlineControl, InputDef, IntValue, NodeBadge, NodeDef,
    OutputDef, PanelSection, PropDef, Socket,
};
use signal_processing::{SimpleTriggerCondition, TriggerProgram};

use crate::nodes::registry::{COLOR_SOURCES, Signal};

/// Selectable sample rates; the stream-mode channel-count constraint limits
/// which are reachable.
const U3_RATES: &[(&str, u64)] = &[
    ("1 MHz", 1_000_000),
    ("2 MHz", 2_000_000),
    ("5 MHz", 5_000_000),
    ("10 MHz", 10_000_000),
    ("20 MHz", 20_000_000),
    ("25 MHz", 25_000_000),
    ("50 MHz", 50_000_000),
    ("100 MHz", 100_000_000),
    ("125 MHz", 125_000_000),
    ("250 MHz", 250_000_000),
    ("500 MHz", 500_000_000),
    ("1 GHz", 1_000_000_000),
];
const U3PRO16_STATE_VERSION: u16 = 3;
pub(crate) const U3PRO16_CHANNELS: usize = 16;

fn u3_rate_names() -> Vec<&'static str> {
    U3_RATES.iter().map(|(name, _)| *name).collect()
}

/// Stream mode by highest selected physical input: ≤16 @ 125 MHz, ≤12 @ 250 MHz,
/// ≤6 @ 500 MHz, and ≤3 @ 1 GHz.
fn u3_max_stream_rate(input_width: usize) -> u64 {
    match input_width {
        0..=3 => 1_000_000_000,
        4..=6 => 500_000_000,
        7..=12 => 250_000_000,
        _ => 125_000_000,
    }
}

/// Read-only single-line text, recomputed by `on_update` (e.g. the U3Pro16
/// body summary "10 ch @ 250 MHz · 1.0 V").
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LabelValue {
    pub text: String,
}

impl InlineControl for LabelValue {
    fn draw_widget(
        &mut self,
        ui: &mut egui::Ui,
        _label: &str,
        rect: egui::Rect,
        zoom: f32,
        clip_rect: egui::Rect,
    ) -> bool {
        let font = egui::FontId::proportional((11.0 * zoom).clamp(7.0, 14.0));
        ui.painter().with_clip_rect(clip_rect).text(
            egui::Pos2::new(rect.left() + 4.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &self.text,
            font,
            Color32::from_rgb(180, 180, 180),
        );
        false
    }
}

/// The DSView-style 16-channel enable grid, drawn as two rows of eight
/// numbered checkboxes. Sized for a panel row (`panel_height(GRID_HEIGHT)`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelGridValue {
    pub enabled: Vec<bool>,
}

impl ChannelGridValue {
    pub const HEIGHT: f32 = 52.0;

    pub fn new(count: usize, enabled_up_to: usize) -> Self {
        Self {
            enabled: (0..count).map(|i| i < enabled_up_to).collect(),
        }
    }

    pub fn enabled_count(&self) -> usize {
        self.enabled.iter().filter(|enabled| **enabled).count()
    }
}

impl InlineControl for ChannelGridValue {
    fn draw_widget(
        &mut self,
        ui: &mut egui::Ui,
        _label: &str,
        rect: egui::Rect,
        zoom: f32,
        clip_rect: egui::Rect,
    ) -> bool {
        let mut changed = false;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.style_mut().spacing.item_spacing = egui::Vec2::new(4.0 * zoom, 2.0 * zoom);
                for row in 0..self.enabled.len().div_ceil(8) {
                    ui.horizontal(|ui| {
                        for index in row * 8..((row + 1) * 8).min(self.enabled.len()) {
                            if ui
                                .checkbox(&mut self.enabled[index], format!("{index}"))
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                }
            },
        );
        changed
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct U3Pro16State {
    #[serde(flatten)]
    pub metadata: U3Pro16Metadata,
    pub mode: EnumValue,
    pub sample_rate: EnumValue,
    pub duration_ms: IntValue,
    pub recording_start: EnumValue,
    pub trigger_position_percent: IntValue,
    pub retention: EnumValue,
    pub retention_duration_ms: IntValue,
    pub retention_megabytes: IntValue,
    pub trigger_timeout_action: EnumValue,
    pub trigger_timeout_ms: IntValue,
    pub rle: BoolValue,
    pub threshold: FloatValue,
    pub filter: BoolValue,
    pub ext_clock: BoolValue,
    pub clock_edge: EnumValue,
    pub channels: ChannelGridValue,
    pub summary: LabelValue,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct U3Pro16Metadata {
    schema_version: u16,
    trigger_program: Option<TriggerProgram>,
    /// Explanation of an auto-clamped rate, surfaced as a node badge.
    #[serde(skip)]
    clamp_note: Option<String>,
    #[serde(skip)]
    compatibility_warning: Option<String>,
}

impl Default for U3Pro16State {
    fn default() -> Self {
        Self {
            metadata: U3Pro16Metadata {
                schema_version: U3PRO16_STATE_VERSION,
                ..Default::default()
            },
            mode: EnumValue::new(0, &["Stream", "Buffer"]),
            sample_rate: EnumValue::new(9, &u3_rate_names()),
            duration_ms: IntValue::new(1000, 1, 60_000),
            recording_start: EnumValue::new(0, &["Immediate", "Trigger"]),
            trigger_position_percent: IntValue::new(50, 0, 100),
            retention: EnumValue::new(0, &["Everything", "Recent duration", "Recent bytes"]),
            retention_duration_ms: IntValue::new(10_000, 1, i32::MAX),
            retention_megabytes: IntValue::new(1024, 1, i32::MAX),
            trigger_timeout_action: EnumValue::new(0, &["Disabled", "Continue waiting", "Stop"]),
            trigger_timeout_ms: IntValue::new(10_000, 1, i32::MAX),
            rle: BoolValue::new(false),
            threshold: FloatValue::new(1.0, 0.0, 5.0, 0.05),
            filter: BoolValue::new(false),
            ext_clock: BoolValue::new(false),
            clock_edge: EnumValue::new(0, &["Rising", "Falling"]),
            channels: ChannelGridValue::new(U3PRO16_CHANNELS, U3PRO16_CHANNELS),
            summary: LabelValue::default(),
        }
    }
}

impl U3Pro16State {
    pub fn trigger_program(&self) -> Option<&TriggerProgram> {
        self.metadata.trigger_program.as_ref()
    }

    pub fn set_trigger_program(&mut self, program: Option<TriggerProgram>) -> Result<(), String> {
        super::trigger::validate_program(self, program.as_ref())?;
        self.metadata.trigger_program = program;
        self.sync_recording_start_to_trigger_program();
        self.metadata.compatibility_warning = None;
        Ok(())
    }

    pub fn set_trigger_condition(
        &mut self,
        physical_channel: usize,
        condition: SimpleTriggerCondition,
    ) -> Result<(), String> {
        self.metadata.trigger_program =
            super::trigger::set_condition(self, physical_channel, condition)?;
        self.sync_recording_start_to_trigger_program();
        self.metadata.compatibility_warning = None;
        Ok(())
    }

    fn sync_recording_start_to_trigger_program(&mut self) {
        if self.metadata.trigger_program.is_some() {
            self.recording_start.select("Trigger");
        } else {
            self.recording_start.select("Immediate");
        }
    }

    fn retain_enabled_trigger_conditions(&mut self) {
        let had_trigger = self.metadata.trigger_program.is_some();
        if let Ok(program) = super::trigger::retain_enabled_conditions(self) {
            self.metadata.trigger_program = program;
            if had_trigger && self.metadata.trigger_program.is_none() {
                self.recording_start.select("Immediate");
            }
        }
    }
}

#[derive(Deserialize)]
struct SavedU3Pro16State {
    #[serde(default)]
    schema_version: u16,
    mode: EnumValue,
    sample_rate: EnumValue,
    duration_ms: IntValue,
    #[serde(default)]
    recording_start: Option<EnumValue>,
    #[serde(default)]
    trigger_position_percent: Option<IntValue>,
    #[serde(default)]
    retention: Option<EnumValue>,
    #[serde(default)]
    retention_duration_ms: Option<IntValue>,
    #[serde(default)]
    retention_megabytes: Option<IntValue>,
    #[serde(default)]
    trigger_timeout_action: Option<EnumValue>,
    #[serde(default)]
    trigger_timeout_ms: Option<IntValue>,
    rle: BoolValue,
    threshold: FloatValue,
    filter: BoolValue,
    ext_clock: BoolValue,
    clock_edge: EnumValue,
    channels: ChannelGridValue,
    summary: LabelValue,
    #[serde(default)]
    trigger_conditions: Vec<SimpleTriggerCondition>,
    #[serde(default)]
    trigger_program: Option<Value>,
}

impl<'de> Deserialize<'de> for U3Pro16State {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let has_saved_trigger_program = value
            .as_object()
            .is_some_and(|object| object.contains_key("trigger_program"));
        let saved: SavedU3Pro16State = serde_json::from_value(value)
            .map_err(|error| serde::de::Error::custom(error.to_string()))?;
        let mut warnings = Vec::new();
        let recording_start_was_missing = saved.recording_start.is_none();
        if saved.schema_version != U3PRO16_STATE_VERSION {
            warnings.push(format!(
                "updated U3Pro16 settings from schema {} to {}",
                saved.schema_version, U3PRO16_STATE_VERSION
            ));
        }
        let mut trigger_conditions = saved.trigger_conditions;
        if !has_saved_trigger_program && trigger_conditions.len() != U3PRO16_CHANNELS {
            trigger_conditions.resize(U3PRO16_CHANNELS, SimpleTriggerCondition::Ignore);
            trigger_conditions.truncate(U3PRO16_CHANNELS);
            warnings.push(format!(
                "normalized trigger input count to {U3PRO16_CHANNELS}; missing inputs defaulted to Ignore"
            ));
        }
        let mut channels = saved.channels;
        if channels.enabled.len() != U3PRO16_CHANNELS {
            channels.enabled.resize(U3PRO16_CHANNELS, false);
            channels.enabled.truncate(U3PRO16_CHANNELS);
            warnings.push(format!("normalized channel count to {U3PRO16_CHANNELS}"));
        }
        let mut state = Self {
            metadata: U3Pro16Metadata {
                schema_version: U3PRO16_STATE_VERSION,
                ..Default::default()
            },
            mode: saved.mode,
            sample_rate: saved.sample_rate,
            duration_ms: saved.duration_ms,
            recording_start: saved
                .recording_start
                .unwrap_or_else(|| EnumValue::new(0, &["Immediate", "Trigger"])),
            trigger_position_percent: saved
                .trigger_position_percent
                .unwrap_or_else(|| IntValue::new(50, 0, 100)),
            retention: saved.retention.unwrap_or_else(|| {
                EnumValue::new(0, &["Everything", "Recent duration", "Recent bytes"])
            }),
            retention_duration_ms: saved
                .retention_duration_ms
                .unwrap_or_else(|| IntValue::new(10_000, 1, i32::MAX)),
            retention_megabytes: saved
                .retention_megabytes
                .unwrap_or_else(|| IntValue::new(1024, 1, i32::MAX)),
            trigger_timeout_action: saved
                .trigger_timeout_action
                .unwrap_or_else(|| EnumValue::new(0, &["Disabled", "Continue waiting", "Stop"])),
            trigger_timeout_ms: saved
                .trigger_timeout_ms
                .unwrap_or_else(|| IntValue::new(10_000, 1, i32::MAX)),
            rle: saved.rle,
            threshold: saved.threshold,
            filter: saved.filter,
            ext_clock: saved.ext_clock,
            clock_edge: saved.clock_edge,
            channels,
            summary: saved.summary,
        };
        let mut reset_trigger_program = false;
        state.metadata.trigger_program = if has_saved_trigger_program {
            match saved.trigger_program {
                None => None,
                Some(value) => match serde_json::from_value::<TriggerProgram>(value) {
                    Ok(program) => match super::trigger::validate_program(&state, Some(&program)) {
                        Ok(()) => Some(program),
                        Err(error) => {
                            reset_trigger_program = true;
                            warnings.push(format!(
                                "reset incompatible trigger program to free run: {error}"
                            ));
                            None
                        }
                    },
                    Err(error) => {
                        reset_trigger_program = true;
                        warnings.push(format!(
                            "reset malformed trigger program to free run: {error}"
                        ));
                        None
                    }
                },
            }
        } else {
            super::trigger::program_from_conditions(&trigger_conditions, &state.channels.enabled)
                .map_err(serde::de::Error::custom)?
        };
        if recording_start_was_missing || reset_trigger_program {
            state.sync_recording_start_to_trigger_program();
        }
        warnings.sort();
        warnings.dedup();
        state.metadata.compatibility_warning = (!warnings.is_empty()).then(|| warnings.join("; "));
        Ok(state)
    }
}

pub struct DsLogicU3Pro16;
impl NodeDef for DsLogicU3Pro16 {
    type State = U3Pro16State;

    fn name() -> &'static str {
        "DSLogic U3Pro16"
    }
    fn category() -> &'static str {
        "Sources"
    }
    fn color() -> Color32 {
        COLOR_SOURCES
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        (0..16_usize)
            .map(|i| OutputDef::new::<Signal>(format!("Ch {i}")))
            .collect()
    }

    fn state() -> Self::State {
        U3Pro16State::default()
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![PropDef::control("summary", "", |state| &mut state.summary)]
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![
            PanelSection::new(
                "Capture",
                vec![
                    PropDef::control("mode", "Mode", |state| &mut state.mode),
                    PropDef::control("sample_rate", "Sample rate", |state| &mut state.sample_rate),
                    PropDef::control("duration_ms", "Duration (ms)", |state| {
                        &mut state.duration_ms
                    }),
                    PropDef::control("recording_start", "Recording start", |state| {
                        &mut state.recording_start
                    }),
                    PropDef::control("trigger_position_percent", "Pre-trigger (%)", |state| {
                        &mut state.trigger_position_percent
                    }),
                    PropDef::control("retention", "Retention", |state| &mut state.retention),
                    PropDef::control("retention_duration_ms", "Retain duration (ms)", |state| {
                        &mut state.retention_duration_ms
                    }),
                    PropDef::control("retention_megabytes", "Retain size (MiB)", |state| {
                        &mut state.retention_megabytes
                    }),
                    PropDef::control("trigger_timeout_action", "Trigger timeout", |state| {
                        &mut state.trigger_timeout_action
                    }),
                    PropDef::control("trigger_timeout_ms", "Timeout after (ms)", |state| {
                        &mut state.trigger_timeout_ms
                    }),
                    PropDef::control("rle", "RLE compress", |state| &mut state.rle),
                ],
            ),
            PanelSection::new(
                "Signal",
                vec![
                    PropDef::control("threshold", "Threshold (V)", |state| &mut state.threshold),
                    PropDef::control("filter", "Filter targets", |state| &mut state.filter),
                    PropDef::control("ext_clock", "External clock", |state| &mut state.ext_clock),
                    PropDef::control("clock_edge", "Clock edge", |state| &mut state.clock_edge),
                ],
            ),
            PanelSection::new(
                "Channels",
                vec![
                    PropDef::control("channels", "Channels", |state: &mut U3Pro16State| {
                        &mut state.channels
                    })
                    .panel_height(ChannelGridValue::HEIGHT),
                ],
            ),
        ]
    }

    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        state.retain_enabled_trigger_conditions();
        let enabled = state.channels.enabled_count();

        // Channel-count ↔ rate constraint (stream mode only): clamp the rate
        // down and explain why via the badge.
        state.metadata.clamp_note = None;
        if state.mode.index == 0 && enabled > 0 {
            let input_width = state
                .channels
                .enabled
                .iter()
                .rposition(|enabled| *enabled)
                .map_or(0, |channel| channel + 1);
            let max_hz = u3_max_stream_rate(input_width);
            let selected_hz = U3_RATES
                .get(state.sample_rate.index)
                .map_or(0, |(_, hz)| *hz);
            if selected_hz > max_hz {
                let clamped = U3_RATES
                    .iter()
                    .rposition(|(_, hz)| *hz <= max_hz)
                    .unwrap_or(0);
                state.sample_rate.index = clamped;
                state.metadata.clamp_note = Some(format!(
                    "Rate limited to {} for this input selection (stream mode)",
                    U3_RATES[clamped].0
                ));
            }
        }

        for (index, output) in outputs.iter_mut().enumerate() {
            output.visible = state.channels.enabled.get(index).copied().unwrap_or(false);
        }

        state.summary.text = format!(
            "{enabled} ch @ {} · {:.1} V",
            state.sample_rate.selected(),
            state.threshold.value
        );
    }

    fn badge(state: &Self::State) -> Option<NodeBadge> {
        if state.channels.enabled_count() == 0 {
            return Some(NodeBadge::warning("No channels enabled"));
        }
        state
            .metadata
            .compatibility_warning
            .as_ref()
            .or(state.metadata.clamp_note.as_ref())
            .map(NodeBadge::warning)
    }
}

#[cfg(test)]
mod tests {
    use node_graph::NodeDef;
    use signal_processing::SimpleTriggerCondition::{Falling, High, Ignore};

    use super::super::trigger;
    use super::{DsLogicU3Pro16, U3PRO16_CHANNELS, U3Pro16State, u3_max_stream_rate};

    #[test]
    fn streaming_rate_limit_uses_highest_enabled_input_not_population_count() {
        assert_eq!(u3_max_stream_rate(3), 1_000_000_000);
        assert_eq!(u3_max_stream_rate(6), 500_000_000);
        assert_eq!(u3_max_stream_rate(12), 250_000_000);
        assert_eq!(u3_max_stream_rate(16), 125_000_000);
    }

    #[test]
    fn current_state_round_trips_simple_triggers_without_a_warning() {
        let mut state = U3Pro16State::default();
        state.set_trigger_condition(2, High).unwrap();
        state.set_trigger_condition(13, Falling).unwrap();
        state.trigger_position_percent.value = 37;
        state.retention.select("Recent bytes");
        state.retention_megabytes.value = 512;
        state.trigger_timeout_action.select("Stop");
        state.trigger_timeout_ms.value = 750;
        let saved = serde_json::to_value(&state).unwrap();
        let restored: U3Pro16State = serde_json::from_value(saved).unwrap();

        assert_eq!(
            trigger::conditions(&restored).unwrap(),
            trigger::conditions(&state).unwrap()
        );
        assert_eq!(restored.recording_start.selected(), "Trigger");
        assert_eq!(restored.trigger_position_percent.value, 37);
        assert_eq!(restored.retention.selected(), "Recent bytes");
        assert_eq!(restored.retention_megabytes.value, 512);
        assert_eq!(restored.trigger_timeout_action.selected(), "Stop");
        assert_eq!(restored.trigger_timeout_ms.value, 750);
        assert!(DsLogicU3Pro16::badge(&restored).is_none());
    }

    #[test]
    fn legacy_state_migrates_trigger_inputs_with_a_visible_warning() {
        let mut saved = serde_json::to_value(U3Pro16State::default()).unwrap();
        let object = saved.as_object_mut().unwrap();
        object.remove("schema_version");
        object.remove("trigger_program");

        let restored: U3Pro16State = serde_json::from_value(saved).unwrap();

        assert_eq!(
            trigger::conditions(&restored).unwrap(),
            [Ignore; U3PRO16_CHANNELS]
        );
        let warning = DsLogicU3Pro16::badge(&restored).unwrap();
        assert!(warning.text.contains("schema 0"));
        assert!(warning.text.contains("defaulted to Ignore"));
        let current = serde_json::to_value(restored).unwrap();
        assert_eq!(current["schema_version"], 3);
        assert_eq!(current["recording_start"]["value"], "Immediate");
        assert_eq!(current["trigger_position_percent"]["value"], 50);
        assert_eq!(current["retention"]["value"], "Everything");
        assert!(current.get("trigger_conditions").is_none());
        assert!(current["trigger_program"].is_null());
    }

    #[test]
    fn malformed_channel_and_trigger_counts_are_normalized() {
        let mut saved = serde_json::to_value(U3Pro16State::default()).unwrap();
        saved["schema_version"] = serde_json::json!(2);
        saved.as_object_mut().unwrap().remove("trigger_program");
        saved["channels"]["enabled"] = serde_json::json!([true, false]);
        saved["trigger_conditions"] = serde_json::json!(["falling"]);
        let restored: U3Pro16State = serde_json::from_value(saved).unwrap();

        assert_eq!(restored.channels.enabled.len(), U3PRO16_CHANNELS);
        let conditions = trigger::conditions(&restored).unwrap();
        assert_eq!(conditions.len(), U3PRO16_CHANNELS);
        assert_eq!(conditions[0], Falling);
        assert!(DsLogicU3Pro16::badge(&restored).is_some());
    }

    #[test]
    fn incompatible_saved_program_resets_with_a_visible_warning() {
        let mut state = U3Pro16State::default();
        state.set_trigger_condition(3, High).unwrap();
        let mut saved = serde_json::to_value(state).unwrap();
        saved["trigger_program"]["schema_id"] = serde_json::json!("future.engine");

        let restored: U3Pro16State = serde_json::from_value(saved).unwrap();

        assert!(restored.trigger_program().is_none());
        assert_eq!(restored.recording_start.selected(), "Immediate");
        let warning = DsLogicU3Pro16::badge(&restored).unwrap();
        assert!(warning.text.contains("reset incompatible trigger program"));
    }

    #[test]
    fn malformed_saved_program_resets_with_a_visible_warning() {
        let mut state = U3Pro16State::default();
        state.set_trigger_condition(3, High).unwrap();
        let mut saved = serde_json::to_value(state).unwrap();
        saved["trigger_program"] = serde_json::json!({ "stages": "not-an-array" });

        let restored: U3Pro16State = serde_json::from_value(saved).unwrap();

        assert!(restored.trigger_program().is_none());
        assert_eq!(restored.recording_start.selected(), "Immediate");
        let warning = DsLogicU3Pro16::badge(&restored).unwrap();
        assert!(warning.text.contains("reset malformed trigger program"));
    }
}
