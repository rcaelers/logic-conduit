//! `DSLogic U3Pro16` node — USB hardware capture source. No matching
//! `compiler` builder exists yet (there is no runtime driver wired up); the
//! node is editable but not runnable.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{
    BoolValue, EnumValue, FloatValue, InlineControl, InputDef, IntValue, NodeBadge, NodeDef,
    OutputDef, PanelSection, PropDef, Socket,
};

use super::registry::{COLOR_SOURCES, Signal};

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

fn u3_rate_names() -> Vec<&'static str> {
    U3_RATES.iter().map(|(name, _)| *name).collect()
}

/// Stream mode: ≤16 ch @ 125 MHz, ≤12 @ 250 MHz, ≤6 @ 500 MHz, ≤3 @ 1 GHz.
fn u3_max_stream_rate(enabled_channels: usize) -> u64 {
    match enabled_channels {
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
        _zoom: f32,
        clip_rect: egui::Rect,
    ) -> bool {
        let mut changed = false;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.style_mut().spacing.item_spacing = egui::Vec2::new(4.0, 2.0);
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct U3Pro16State {
    pub mode: EnumValue,
    pub sample_rate: EnumValue,
    pub duration_ms: IntValue,
    pub rle: BoolValue,
    pub threshold: FloatValue,
    pub filter: BoolValue,
    pub ext_clock: BoolValue,
    pub clock_edge: EnumValue,
    pub channels: ChannelGridValue,
    pub summary: LabelValue,
    /// Explanation of an auto-clamped rate, surfaced as a node badge.
    #[serde(skip)]
    pub clamp_note: Option<String>,
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
        U3Pro16State {
            mode: EnumValue::new(0, &["Stream", "Buffer"]),
            sample_rate: EnumValue::new(9, &u3_rate_names()),
            duration_ms: IntValue::new(1000, 1, 60_000),
            rle: BoolValue::new(false),
            threshold: FloatValue::new(1.0, 0.0, 5.0, 0.05),
            filter: BoolValue::new(false),
            ext_clock: BoolValue::new(false),
            clock_edge: EnumValue::new(0, &["Rising", "Falling"]),
            channels: ChannelGridValue::new(16, 16),
            summary: LabelValue::default(),
            clamp_note: None,
        }
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
        let enabled = state.channels.enabled_count();

        // Channel-count ↔ rate constraint (stream mode only): clamp the rate
        // down and explain why via the badge.
        state.clamp_note = None;
        if state.mode.index == 0 && enabled > 0 {
            let max_hz = u3_max_stream_rate(enabled);
            let selected_hz = U3_RATES
                .get(state.sample_rate.index)
                .map_or(0, |(_, hz)| *hz);
            if selected_hz > max_hz {
                let clamped = U3_RATES
                    .iter()
                    .rposition(|(_, hz)| *hz <= max_hz)
                    .unwrap_or(0);
                state.sample_rate.index = clamped;
                state.clamp_note = Some(format!(
                    "Rate limited to {} for {enabled} channels (stream mode)",
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
        state.clamp_note.as_ref().map(NodeBadge::warning)
    }
}
