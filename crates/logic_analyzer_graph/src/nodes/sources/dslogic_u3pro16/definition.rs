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
const MAX_STREAM_SAMPLES: u64 = 1 << 34;
const U3PRO16_STATE_VERSION: u16 = 4;
pub(crate) const U3PRO16_CHANNELS: usize = 16;

fn u3_rate_names() -> Vec<&'static str> {
    U3_RATES.iter().map(|(name, _)| *name).collect()
}

fn u3_max_rate(mode: &str, input_width: usize) -> u64 {
    if mode == "Buffer" {
        if input_width <= 8 {
            1_000_000_000
        } else {
            500_000_000
        }
    } else {
        match input_width {
            0..=3 => 1_000_000_000,
            4..=6 => 500_000_000,
            7..=12 => 250_000_000,
            _ => 125_000_000,
        }
    }
}

fn u3_input_width_limit(mode: &str, sample_rate_hz: u64) -> usize {
    if mode == "Buffer" {
        if sample_rate_hz <= 500_000_000 { 16 } else { 8 }
    } else {
        match sample_rate_hz {
            0..=125_000_000 => 16,
            125_000_001..=250_000_000 => 12,
            250_000_001..=500_000_000 => 6,
            _ => 3,
        }
    }
}

pub(crate) fn channel_rate_validation_error(state: &U3Pro16State) -> Option<String> {
    let mode = state.mode.selected();
    let input_width = state
        .channels
        .enabled
        .iter()
        .rposition(|enabled| *enabled)
        .map_or(0, |channel| channel + 1);
    if input_width == 0 {
        return None;
    }
    let (rate_name, sample_rate_hz) = U3_RATES
        .get(state.sample_rate.index)
        .copied()
        .unwrap_or(U3_RATES[0]);
    if sample_rate_hz <= u3_max_rate(mode, input_width) {
        return None;
    }
    let width_limit = u3_input_width_limit(mode, sample_rate_hz);
    Some(format!(
        "Too many channels for {rate_name} in {mode} mode: highest selected is Ch {}; enable only Ch 0–{} or lower the sample rate",
        input_width - 1,
        width_limit - 1,
    ))
}

pub(crate) fn capture_duration_limit_ns(
    mode: &str,
    sample_rate_hz: u64,
    enabled_channels: usize,
) -> u64 {
    let max_samples = if mode == "Stream" {
        MAX_STREAM_SAMPLES
    } else {
        (2_u64 * 1024 * 1024 * 1024 / enabled_channels.max(1) as u64) & !1023
    };
    ((u128::from(max_samples) * 1_000_000_000) / u128::from(sample_rate_hz.max(1)))
        .min(u128::from(u64::MAX)) as u64
}

fn duration_presets(max_nanoseconds: u64) -> Vec<u64> {
    let mut presets = Vec::new();
    let mut scale = CaptureDurationValue::MIN_NS;
    while scale <= max_nanoseconds {
        for multiplier in [1_u64, 2, 5] {
            let Some(candidate) = scale.checked_mul(multiplier) else {
                break;
            };
            if candidate > max_nanoseconds {
                break;
            }
            presets.push(candidate);
        }
        let Some(next) = scale.checked_mul(10) else {
            break;
        };
        scale = next;
    }
    if presets.last().copied() != Some(max_nanoseconds) {
        presets.push(max_nanoseconds);
    }
    presets
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureDurationValue {
    nanoseconds: u64,
    #[serde(skip, default = "CaptureDurationValue::default_max_nanoseconds")]
    max_nanoseconds: u64,
}

impl CaptureDurationValue {
    const MIN_NS: u64 = 10_000;
    const fn default_max_nanoseconds() -> u64 {
        u64::MAX
    }

    pub fn from_nanoseconds(nanoseconds: u64) -> Self {
        Self {
            nanoseconds: nanoseconds.max(Self::MIN_NS),
            max_nanoseconds: Self::default_max_nanoseconds(),
        }
    }

    pub fn from_milliseconds(milliseconds: u64) -> Self {
        Self::from_nanoseconds(milliseconds.saturating_mul(1_000_000))
    }

    pub fn set_milliseconds(&mut self, milliseconds: u64) {
        *self = Self::from_milliseconds(milliseconds);
    }

    pub fn nanoseconds(&self) -> u64 {
        self.nanoseconds
    }

    fn set_max_nanoseconds(&mut self, max_nanoseconds: u64) {
        self.max_nanoseconds = max_nanoseconds.max(Self::MIN_NS);
        self.nanoseconds = self.nanoseconds.clamp(Self::MIN_NS, self.max_nanoseconds);
    }
}

fn format_duration(nanoseconds: u64) -> String {
    if nanoseconds >= 3_600_000_000_000 {
        format!("{:.2} h", nanoseconds as f64 / 3_600_000_000_000.0)
    } else if nanoseconds >= 60_000_000_000 {
        format!("{:.2} min", nanoseconds as f64 / 60_000_000_000.0)
    } else if nanoseconds >= 1_000_000_000 && !nanoseconds.is_multiple_of(1_000_000_000) {
        format!("{:.2} s", nanoseconds as f64 / 1_000_000_000.0)
    } else if nanoseconds.is_multiple_of(1_000_000_000) {
        format!("{} s", nanoseconds / 1_000_000_000)
    } else if nanoseconds.is_multiple_of(1_000_000) {
        format!("{} ms", nanoseconds / 1_000_000)
    } else if nanoseconds.is_multiple_of(1_000) {
        format!("{} µs", nanoseconds / 1_000)
    } else {
        format!("{nanoseconds} ns")
    }
}

impl InlineControl for CaptureDurationValue {
    fn draw_widget(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        rect: egui::Rect,
        zoom: f32,
        clip_rect: egui::Rect,
    ) -> bool {
        let old = self.nanoseconds;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.style_mut().spacing.item_spacing = egui::Vec2::splat(2.0 * zoom);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).size(10.0 * zoom));
                    egui::ComboBox::from_id_salt("capture-duration")
                        .selected_text(format_duration(self.nanoseconds))
                        .show_ui(ui, |ui| {
                            for duration in duration_presets(self.max_nanoseconds) {
                                ui.selectable_value(
                                    &mut self.nanoseconds,
                                    duration,
                                    format_duration(duration),
                                );
                            }
                        });
                });
            },
        );
        self.nanoseconds != old
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
    #[serde(skip)]
    pub supported_width: Option<usize>,
    #[serde(skip)]
    pub validation_error: Option<String>,
    #[serde(skip)]
    pub selection_anchor: Option<(usize, bool)>,
    #[serde(skip)]
    pub drag_value: Option<bool>,
}

impl ChannelGridValue {
    const COLUMNS: usize = 8;
    const ROW_HEIGHT: f32 = 52.0;
    const ERROR_HEIGHT: f32 = 42.0;
    const HORIZONTAL_INSET: f32 = 8.0;
    pub const HEIGHT: f32 = Self::ROW_HEIGHT * 2.0 + Self::ERROR_HEIGHT;

    pub fn new(count: usize, enabled_up_to: usize) -> Self {
        Self {
            enabled: (0..count).map(|i| i < enabled_up_to).collect(),
            supported_width: None,
            validation_error: None,
            selection_anchor: None,
            drag_value: None,
        }
    }

    pub fn enabled_count(&self) -> usize {
        self.enabled.iter().filter(|enabled| **enabled).count()
    }

    fn apply_click(&mut self, index: usize, shift: bool) -> bool {
        if index >= self.enabled.len() {
            return false;
        }
        if shift && let Some((anchor, value)) = self.selection_anchor {
            let range = anchor.min(index)..=anchor.max(index);
            let changed = self.enabled[range.clone()]
                .iter()
                .any(|enabled| *enabled != value);
            self.enabled[range].fill(value);
            return changed;
        }
        let value = !self.enabled[index];
        self.enabled[index] = value;
        self.selection_anchor = Some((index, value));
        true
    }

    fn begin_drag(&mut self, index: usize) -> bool {
        if index >= self.enabled.len() {
            return false;
        }
        let value = !self.enabled[index];
        self.enabled[index] = value;
        self.selection_anchor = Some((index, value));
        self.drag_value = Some(value);
        true
    }

    fn continue_drag(&mut self, index: usize) -> bool {
        let Some(value) = self.drag_value else {
            return false;
        };
        let Some(enabled) = self.enabled.get_mut(index) else {
            return false;
        };
        let changed = *enabled != value;
        *enabled = value;
        changed
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
        let supported_width = self.supported_width;
        let validation_error = self.validation_error.clone();
        let pointer_down = ui.input(|input| input.pointer.primary_down());
        let pointer_pos = ui.input(|input| input.pointer.interact_pos());
        let shift = ui.input(|input| input.modifiers.shift);
        if !pointer_down || !ui.is_enabled() {
            self.drag_value = None;
        }
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                let grid_rect = rect.shrink2(egui::Vec2::new(Self::HORIZONTAL_INSET, 0.0));
                let column_width = grid_rect.width() / Self::COLUMNS as f32;
                for index in 0..self.enabled.len() {
                    let row = index / Self::COLUMNS;
                    let column = index % Self::COLUMNS;
                    let cell = egui::Rect::from_min_size(
                        egui::Pos2::new(
                            grid_rect.left() + column as f32 * column_width,
                            grid_rect.top() + row as f32 * Self::ROW_HEIGHT,
                        ),
                        egui::Vec2::new(column_width, Self::ROW_HEIGHT),
                    );
                    ui.push_id(("channel", index), |ui| {
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(cell)
                                .layout(egui::Layout::top_down(egui::Align::Center)),
                            |ui| {
                                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                                ui.style_mut().spacing.item_spacing.y = 3.0 * zoom;
                                let invalid = self.enabled[index]
                                    && supported_width.is_some_and(|width| index >= width);
                                let mut number =
                                    egui::RichText::new(index.to_string()).size(13.0 * zoom);
                                if invalid {
                                    number = number.color(ui.visuals().error_fg_color);
                                }
                                ui.label(number);
                                let size = 20.0 * zoom;
                                let (toggle, response) = ui.allocate_exact_size(
                                    egui::Vec2::splat(size),
                                    egui::Sense::click_and_drag(),
                                );
                                if response.drag_started() {
                                    changed |= self.begin_drag(index);
                                } else if response.clicked() {
                                    changed |= self.apply_click(index, shift);
                                }
                                if pointer_down
                                    && self.drag_value.is_some()
                                    && pointer_pos.is_some_and(|pointer| cell.contains(pointer))
                                {
                                    changed |= self.continue_drag(index);
                                }
                                let visuals = ui.style().interact(&response);
                                let painter = ui.painter().with_clip_rect(clip_rect);
                                painter.rect_filled(toggle, 0.0, Color32::from_rgb(18, 18, 18));
                                let outline = if invalid {
                                    ui.visuals().error_fg_color
                                } else {
                                    visuals.fg_stroke.color
                                };
                                painter.rect_stroke(
                                    toggle,
                                    0.0,
                                    egui::Stroke::new(1.5 * zoom, outline),
                                    egui::StrokeKind::Inside,
                                );
                                if self.enabled[index] {
                                    let fill = if invalid {
                                        ui.visuals().error_fg_color
                                    } else {
                                        ui.visuals().selection.bg_fill
                                    };
                                    painter.rect_filled(toggle.shrink(4.0 * zoom), 0.0, fill);
                                }
                                response.on_hover_text(format!(
                                    "Channel {index}\nClick to toggle · Shift-click for range · Drag to paint"
                                ));
                            },
                        );
                    });
                }
                if let Some(error) = &validation_error {
                    let error_rect = egui::Rect::from_min_max(
                        egui::Pos2::new(
                            rect.left() + Self::HORIZONTAL_INSET,
                            rect.top() + Self::ROW_HEIGHT * 2.0,
                        ),
                        egui::Pos2::new(rect.right() - Self::HORIZONTAL_INSET, rect.bottom()),
                    );
                    ui.scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(error_rect)
                            .layout(egui::Layout::top_down(egui::Align::LEFT)),
                        |ui| {
                            ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(error)
                                        .size(10.5 * zoom)
                                        .color(ui.visuals().error_fg_color),
                                )
                                .wrap(),
                            );
                        },
                    );
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
    pub duration: CaptureDurationValue,
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
    #[serde(skip)]
    configuration_error: Option<String>,
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
            sample_rate: EnumValue::new(8, &u3_rate_names()),
            duration: CaptureDurationValue::from_milliseconds(1_000),
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
    #[serde(default)]
    duration: Option<CaptureDurationValue>,
    #[serde(default)]
    duration_ms: Option<IntValue>,
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
            duration: saved
                .duration
                .map(|duration| CaptureDurationValue::from_nanoseconds(duration.nanoseconds()))
                .unwrap_or_else(|| {
                    CaptureDurationValue::from_milliseconds(
                        saved
                            .duration_ms
                            .as_ref()
                            .map_or(1_000, |duration| duration.value.max(1) as u64),
                    )
                }),
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
            .map(|i| OutputDef::new::<Signal>(format!("Ch {i}")).view_selectable(false))
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
                    PropDef::control("duration", "Duration", |state| &mut state.duration),
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

        // Keep an invalid selection intact so the user can correct either the
        // rate or channels deliberately; surface the same validation error
        // that prevents capture startup.
        let sample_rate_hz = U3_RATES
            .get(state.sample_rate.index)
            .map_or(U3_RATES[0].1, |(_, hz)| *hz);
        let configuration_error = channel_rate_validation_error(state);
        state.channels.supported_width = configuration_error
            .as_ref()
            .map(|_| u3_input_width_limit(state.mode.selected(), sample_rate_hz));
        state.channels.validation_error = configuration_error.clone();
        state.metadata.configuration_error = configuration_error;
        state
            .duration
            .set_max_nanoseconds(capture_duration_limit_ns(
                state.mode.selected(),
                sample_rate_hz,
                enabled,
            ));

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
        if let Some(error) = &state.metadata.configuration_error {
            return Some(NodeBadge::error(error));
        }
        state
            .metadata
            .compatibility_warning
            .as_ref()
            .map(NodeBadge::warning)
    }
}

#[cfg(test)]
mod tests {
    use node_graph::{BadgeSeverity, NodeDef};
    use signal_processing::SimpleTriggerCondition::{Falling, High, Ignore};

    use super::super::trigger;
    use super::{
        CaptureDurationValue, ChannelGridValue, DsLogicU3Pro16, U3PRO16_CHANNELS, U3Pro16State,
        capture_duration_limit_ns, format_duration, u3_max_rate,
    };

    #[test]
    fn shift_click_applies_the_anchor_value_to_the_inclusive_range() {
        let mut channels = ChannelGridValue::new(8, 8);

        assert!(channels.apply_click(1, false));
        assert!(channels.apply_click(5, true));

        assert!(channels.enabled[0]);
        assert_eq!(&channels.enabled[1..=5], &[false; 5]);
        assert!(channels.enabled[6]);
        assert!(channels.enabled[7]);

        channels.enabled.fill(false);
        assert!(channels.apply_click(2, false));
        assert!(channels.apply_click(6, true));
        assert_eq!(&channels.enabled[2..=6], &[true; 5]);
    }

    #[test]
    fn drag_paints_the_initial_toggle_value_across_crossed_channels() {
        let mut channels = ChannelGridValue::new(8, 0);

        assert!(channels.begin_drag(2));
        assert!(channels.continue_drag(3));
        assert!(channels.continue_drag(5));
        assert!(!channels.continue_drag(5));

        assert_eq!(
            channels.enabled,
            [false, false, true, true, false, true, false, false]
        );
    }

    #[test]
    fn capture_durations_use_human_scale_units() {
        assert_eq!(format_duration(10_000), "10 µs");
        assert_eq!(format_duration(10_000_000), "10 ms");
        assert_eq!(format_duration(2_000_000_000), "2 s");
    }

    #[test]
    fn streaming_duration_limit_matches_dsview_sample_depth() {
        assert_eq!(
            format_duration(capture_duration_limit_ns("Stream", 1_000_000, 16)),
            "4.77 h"
        );
        assert_eq!(
            format_duration(capture_duration_limit_ns("Stream", 2_000_000, 16)),
            "2.39 h"
        );
        assert_eq!(
            format_duration(capture_duration_limit_ns("Stream", 100_000_000, 16)),
            "2.86 min"
        );
        assert_eq!(
            format_duration(capture_duration_limit_ns("Stream", 125_000_000, 16)),
            "2.29 min"
        );
        assert_eq!(
            format_duration(capture_duration_limit_ns("Stream", 1_000_000_000, 3)),
            "17.18 s"
        );
    }

    #[test]
    fn buffered_duration_limit_increases_with_fewer_channels() {
        let sixteen = capture_duration_limit_ns("Buffer", 100_000_000, 16);
        let eight = capture_duration_limit_ns("Buffer", 100_000_000, 8);

        assert_eq!(eight, sixteen * 2);
    }

    #[test]
    fn rate_limit_uses_mode_and_highest_enabled_input() {
        assert_eq!(u3_max_rate("Stream", 3), 1_000_000_000);
        assert_eq!(u3_max_rate("Stream", 6), 500_000_000);
        assert_eq!(u3_max_rate("Stream", 12), 250_000_000);
        assert_eq!(u3_max_rate("Stream", 16), 125_000_000);
        assert_eq!(u3_max_rate("Buffer", 8), 1_000_000_000);
        assert_eq!(u3_max_rate("Buffer", 16), 500_000_000);
    }

    #[test]
    fn invalid_channel_selection_is_an_error_and_does_not_change_the_rate() {
        let mut state = U3Pro16State::default();
        state.sample_rate.select("1 GHz");
        let mut inputs = [];
        let mut outputs = [];

        DsLogicU3Pro16::on_update(&mut state, &mut inputs, &mut outputs);

        assert_eq!(state.sample_rate.selected(), "1 GHz");
        let badge = DsLogicU3Pro16::badge(&state).unwrap();
        assert_eq!(badge.severity, BadgeSeverity::Error);
        assert!(badge.text.contains("Too many channels"));
        assert!(badge.text.contains("Ch 0–2"));
        assert_eq!(state.channels.supported_width, Some(3));
        assert_eq!(
            state.channels.validation_error.as_deref(),
            Some(badge.text.as_str())
        );

        state.channels.enabled.fill(false);
        state.channels.enabled[..3].fill(true);
        DsLogicU3Pro16::on_update(&mut state, &mut inputs, &mut outputs);
        assert!(DsLogicU3Pro16::badge(&state).is_none());
        assert_eq!(state.channels.supported_width, None);
        assert_eq!(state.channels.validation_error, None);

        state.mode.select("Buffer");
        state.channels.enabled.fill(true);
        DsLogicU3Pro16::on_update(&mut state, &mut inputs, &mut outputs);
        let badge = DsLogicU3Pro16::badge(&state).unwrap();
        assert_eq!(badge.severity, BadgeSeverity::Error);
        assert!(badge.text.contains("Buffer mode"));
        assert!(badge.text.contains("Ch 0–7"));
        assert_eq!(state.channels.supported_width, Some(8));
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
        assert_eq!(current["schema_version"], 4);
        assert_eq!(current["recording_start"]["value"], "Immediate");
        assert_eq!(current["trigger_position_percent"]["value"], 50);
        assert_eq!(current["retention"]["value"], "Everything");
        assert!(current.get("trigger_conditions").is_none());
        assert!(current["trigger_program"].is_null());
    }

    #[test]
    fn millisecond_capture_duration_migrates_without_losing_its_value() {
        let mut saved = serde_json::to_value(U3Pro16State::default()).unwrap();
        saved["schema_version"] = serde_json::json!(3);
        saved.as_object_mut().unwrap().remove("duration");
        saved["duration_ms"] =
            serde_json::to_value(node_graph::IntValue::new(2_000, 1, 60_000)).unwrap();

        let restored: U3Pro16State = serde_json::from_value(saved).unwrap();

        assert_eq!(restored.duration.nanoseconds(), 2_000_000_000);
        assert_eq!(format_duration(restored.duration.nanoseconds()), "2 s");
        assert!(DsLogicU3Pro16::badge(&restored).is_some());
        assert_eq!(
            CaptureDurationValue::from_milliseconds(2_000).nanoseconds(),
            restored.duration.nanoseconds()
        );
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
