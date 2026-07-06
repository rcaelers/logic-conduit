//! Node definitions for the analysis-pipeline editor.
//!
//! Socket styling follows `ANALYSIS_PIPELINE_DESIGN.md` §3.2: the shape
//! encodes how a value exists in time (■ static config, ● level stream,
//! ◆ event stream) and the color encodes the payload family, shared across
//! shapes (green logic, amber pulse, orange words, blue integer, rose text).
//! Red is reserved for error feedback, grey for the wildcard.
//!
//! Prop placement follows §4.12: the node body carries sockets and the
//! controls someone tweaks while reading the graph; everything else lives in
//! the properties panel (N).

use egui::Color32;
use node_graph::{
    BoolValue, EnumValue, FileValue, FloatValue, InlineControl, InputDef, IntValue, NodeBadge,
    NodeDef, NodeGraphWidget, NodeTypeRegistry, OutputDef, PanelSection, PropDef, Socket,
    SocketDef, SocketDirection, SocketId, SocketShape, StringValue,
};
use serde::{Deserialize, Serialize};

// ── Stream socket types (§3.3) ───────────────────────────────────────────────

/// Logic level stream (`Sample` at runtime): defined at every instant.
pub struct Signal;
impl SocketDef for Signal {
    type Value = bool;

    fn type_name() -> &'static str {
        "Signal"
    }
    fn color() -> Color32 {
        Color32::from_rgb(95, 175, 95)
    }
}

/// Decoded word events (`SpiTransfer` / `ParallelWord` at runtime).
pub struct Words;
impl SocketDef for Words {
    type Value = u64;

    fn type_name() -> &'static str {
        "Words"
    }
    fn color() -> Color32 {
        Color32::from_rgb(215, 140, 60)
    }
    fn shape() -> SocketShape {
        SocketShape::Diamond
    }
}

/// Instantaneous events with no payload beyond time (`Trigger` at runtime).
pub struct Trigger;
impl SocketDef for Trigger {
    type Value = ();

    fn type_name() -> &'static str {
        "Trigger"
    }
    fn color() -> Color32 {
        Color32::from_rgb(230, 190, 80)
    }
    fn shape() -> SocketShape {
        SocketShape::Diamond
    }
}

/// Integer level stream (`NumberSample` at runtime).
pub struct Number;
impl SocketDef for Number {
    type Value = i64;

    fn type_name() -> &'static str {
        "Number"
    }
    fn color() -> Color32 {
        Color32::from_rgb(95, 145, 210)
    }
}

/// Text level stream (`TextSample` at runtime).
pub struct Text;
impl SocketDef for Text {
    type Value = String;

    fn type_name() -> &'static str {
        "Text"
    }
    fn color() -> Color32 {
        Color32::from_rgb(215, 150, 170)
    }
}

// ── Category colors ──────────────────────────────────────────────────────────

const COLOR_SOURCES: Color32 = Color32::from_rgb(100, 75, 140);
const COLOR_DECODERS: Color32 = Color32::from_rgb(60, 100, 160);
const COLOR_LOGIC: Color32 = Color32::from_rgb(60, 140, 100);
const COLOR_OUTPUT: Color32 = Color32::from_rgb(160, 80, 60);

// ── Custom inline controls ───────────────────────────────────────────────────

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

fn parse_hex(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(digits, 16).ok()
}

// ── DSL File Source ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DslFileSourceState {
    pub file: FileValue,
    pub channels: IntValue,
}

pub struct DslFileSource;
impl NodeDef for DslFileSource {
    type State = DslFileSourceState;

    fn name() -> &'static str {
        "DSL File Source"
    }
    fn category() -> &'static str {
        "Sources"
    }
    fn color() -> Color32 {
        COLOR_SOURCES
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::control::<node_graph::FileSocket>("File", |state| &mut state.file),
            InputDef::control::<node_graph::IntSocket>("Channels", |state| &mut state.channels),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        (0..32_usize)
            .map(|i| OutputDef::new::<Signal>(format!("Ch {i}")))
            .collect()
    }

    fn state() -> Self::State {
        DslFileSourceState {
            file: FileValue::with_filter(
                "_captures/wipneus5.dsl",
                "Select DSLogic capture",
                "DSLogic captures",
                &["dsl"],
            ),
            channels: IntValue::new(11, 1, 32),
        }
    }

    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        let channels = (state.channels.value as usize).clamp(1, 32);
        for (index, output) in outputs.iter_mut().enumerate() {
            output.visible = index < channels;
        }
    }
}

// ── DSLogic U3Pro16 Source (§4.10) ───────────────────────────────────────────

/// Selectable sample rates; the stream-mode channel-count constraint limits
/// which are reachable (§4.10).
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

// ── SPI Decoder (§4.1) ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpiDecoderState {
    pub word_size: IntValue,
    pub cpol: EnumValue,
    pub cpha: EnumValue,
    pub bit_order: EnumValue,
    pub cs_polarity: EnumValue,
    pub has_miso: BoolValue,
}

pub struct SpiDecoder;
impl NodeDef for SpiDecoder {
    type State = SpiDecoderState;

    fn name() -> &'static str {
        "SPI Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        COLOR_DECODERS
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("CLK"),
            InputDef::new::<Signal>("MOSI"),
            InputDef::new::<Signal>("MISO"),
            InputDef::new::<Signal>("CS#"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![
            OutputDef::new::<Words>("MOSI Words"),
            OutputDef::new::<Words>("MISO Words"),
        ]
    }

    fn state() -> Self::State {
        SpiDecoderState {
            word_size: IntValue::new(8, 1, 32),
            cpol: EnumValue::new(0, &["0", "1"]),
            cpha: EnumValue::new(0, &["0", "1"]),
            bit_order: EnumValue::new(0, &["MSB first", "LSB first"]),
            cs_polarity: EnumValue::new(0, &["Active low", "Active high", "Disabled"]),
            has_miso: BoolValue::new(true),
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![PropDef::control("word_size", "Word size", |state| {
            &mut state.word_size
        })]
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("cpol", "CPOL", |state| &mut state.cpol),
                PropDef::control("cpha", "CPHA", |state| &mut state.cpha),
                PropDef::control("bit_order", "Bit order", |state| &mut state.bit_order),
                PropDef::control("cs_polarity", "CS# polarity", |state| &mut state.cs_polarity),
                PropDef::control("has_miso", "Has MISO", |state| &mut state.has_miso),
            ],
        )]
    }

    fn on_update(state: &mut Self::State, inputs: &mut [Socket], outputs: &mut [Socket]) {
        if let Some(miso) = inputs.get_mut(2) {
            miso.visible = state.has_miso.value;
        }
        if let Some(miso_words) = outputs.get_mut(1) {
            miso_words.visible = state.has_miso.value;
        }
    }
}

// ── UART Decoder (§4.13, single line) ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UartDecoderState {
    pub baud_rate: IntValue,
    pub data_bits: IntValue,
    pub parity: EnumValue,
    pub check_parity: BoolValue,
    pub stop_bits: EnumValue,
    pub bit_order: EnumValue,
    pub invert: BoolValue,
    pub error_output: BoolValue,
}

pub struct UartDecoder;
impl NodeDef for UartDecoder {
    type State = UartDecoderState;

    fn name() -> &'static str {
        "UART Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        COLOR_DECODERS
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("RX/TX"),
            InputDef::control::<node_graph::IntSocket>("Baud Rate", |state| &mut state.baud_rate),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![
            OutputDef::new::<Words>("Words"),
            OutputDef::new::<Trigger>("Error"),
        ]
    }

    fn state() -> Self::State {
        UartDecoderState {
            baud_rate: IntValue::new(1_000_000, 300, 100_000_000),
            data_bits: IntValue::new(8, 5, 9),
            parity: EnumValue::new(0, &["None", "Odd", "Even", "Mark", "Space"]),
            check_parity: BoolValue::new(false),
            stop_bits: EnumValue::new(2, &["0", "0.5", "1", "1.5", "2"]),
            bit_order: EnumValue::new(0, &["LSB first", "MSB first"]),
            invert: BoolValue::new(false),
            error_output: BoolValue::new(false),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("data_bits", "Data bits", |state| &mut state.data_bits),
                PropDef::control("parity", "Parity", |state| &mut state.parity),
                PropDef::control("check_parity", "Check parity", |state| &mut state.check_parity),
                PropDef::control("stop_bits", "Stop bits", |state| &mut state.stop_bits),
                PropDef::control("bit_order", "Bit order", |state| &mut state.bit_order),
                PropDef::control("invert", "Invert signal", |state| &mut state.invert),
                PropDef::control("error_output", "Error output", |state| {
                    &mut state.error_output
                }),
            ],
        )]
    }

    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        if let Some(error) = outputs.get_mut(1) {
            error.visible = state.error_output.value;
        }
    }
}

// ── I2C Decoder (demo placeholder) ───────────────────────────────────────────

pub struct I2cDecoder;
impl NodeDef for I2cDecoder {
    type State = ();

    fn name() -> &'static str {
        "I2C Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        COLOR_DECODERS
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("SCL"),
            InputDef::new::<Signal>("SDA"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Words>("Words")]
    }

    fn state() -> Self::State {}
}

// ── Binary Decoder (§4.5) ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryDecoderState {
    pub sample_on: EnumValue,
    pub word_size: IntValue,
    pub endianness: EnumValue,
    pub cs_polarity: EnumValue,
}

pub struct BinaryDecoder;
impl NodeDef for BinaryDecoder {
    type State = BinaryDecoderState;

    fn name() -> &'static str {
        "Binary Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        COLOR_DECODERS
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("Clock"),
            InputDef::new::<Signal>("D").variadic(32),
            InputDef::new::<Signal>("CS"),
            InputDef::new::<Signal>("Enable"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Words>("Words")]
    }

    fn state() -> Self::State {
        BinaryDecoderState {
            sample_on: EnumValue::new(
                0,
                &[
                    "Rising (SDR)",
                    "Falling (SDR)",
                    "Both (DDR)",
                    "High level",
                    "Low level",
                ],
            ),
            word_size: IntValue::new(1, 1, 8),
            endianness: EnumValue::new(0, &["Little", "Big"]),
            cs_polarity: EnumValue::new(0, &["Disabled", "Active low", "Active high"]),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("sample_on", "Sample on", |state| &mut state.sample_on),
                PropDef::control("word_size", "Word size (cycles)", |state| {
                    &mut state.word_size
                }),
                PropDef::control("endianness", "Endianness", |state| &mut state.endianness),
                PropDef::control("cs_polarity", "CS polarity", |state| &mut state.cs_polarity),
            ],
        )]
    }
}

// ── Word Matcher (§4.2) ──────────────────────────────────────────────────────

pub const MATCH_OPS: &[&str] = &["==", "≠", "<", "≤", ">", "≥"];

fn default_match_op() -> EnumValue {
    EnumValue::new(0, MATCH_OPS)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordMatcherState {
    pub pattern: StringValue,
    pub mask: StringValue,
    /// Comparison of the masked word against the masked pattern.
    #[serde(default = "default_match_op")]
    pub op: EnumValue,
    pub field: EnumValue,
    pub pulse_output: BoolValue,
}

pub struct WordMatcher;
impl NodeDef for WordMatcher {
    type State = WordMatcherState;

    fn name() -> &'static str {
        "Word Matcher"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![InputDef::new::<Words>("Words")]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![
            OutputDef::new::<Trigger>("Match"),
            OutputDef::new::<Signal>("Matched"),
        ]
    }

    fn state() -> Self::State {
        WordMatcherState {
            pattern: StringValue::new("0x000000"),
            mask: StringValue::new("0xFFFFFF"),
            op: default_match_op(),
            field: EnumValue::new(0, &["MOSI", "MISO"]),
            pulse_output: BoolValue::new(false),
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![PropDef::control("pattern", "Pattern", |state| {
            &mut state.pattern
        })]
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("op", "Compare", |state| &mut state.op),
                PropDef::control("mask", "Mask", |state| &mut state.mask),
                PropDef::control("field", "Field", |state| &mut state.field),
                PropDef::control("pulse_output", "Pulse output", |state| {
                    &mut state.pulse_output
                }),
            ],
        )]
    }

    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        if let Some(matched) = outputs.get_mut(1) {
            matched.visible = state.pulse_output.value;
        }
    }

    fn badge(state: &Self::State) -> Option<NodeBadge> {
        if parse_hex(&state.pattern.value).is_none() {
            return Some(NodeBadge::error("Invalid hex pattern"));
        }
        if parse_hex(&state.mask.value).is_none() {
            return Some(NodeBadge::error("Invalid hex mask"));
        }
        None
    }
}

// ── SR Flip-Flop (§4.3) ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrFlipFlopState {
    pub initial: BoolValue,
}

pub struct SrFlipFlop;
impl NodeDef for SrFlipFlop {
    type State = SrFlipFlopState;

    fn name() -> &'static str {
        "SR Flip-Flop"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Trigger>("Set"),
            InputDef::new::<Trigger>("Reset"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Signal>("Q")]
    }

    fn state() -> Self::State {
        SrFlipFlopState {
            initial: BoolValue::new(false),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![PropDef::control("initial", "Initial state", |state| {
                &mut state.initial
            })],
        )]
    }
}

// ── Logic Gate (§4.4) ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicGateState {
    pub op: EnumValue,
    /// Set by `on_update` when extra inputs are connected to a NOT gate.
    #[serde(skip)]
    pub note: Option<String>,
}

pub struct LogicGate;
impl NodeDef for LogicGate {
    type State = LogicGateState;

    fn name() -> &'static str {
        "Logic Gate"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![InputDef::new::<Signal>("In").variadic(8)]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Signal>("Out")]
    }

    fn state() -> Self::State {
        LogicGateState {
            op: EnumValue::new(1, &["NOT", "AND", "NAND", "OR", "NOR", "XOR", "XNOR"]),
            note: None,
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![PropDef::control("op", "Op", |state| &mut state.op)]
    }

    fn on_update(state: &mut Self::State, inputs: &mut [Socket], _outputs: &mut [Socket]) {
        let is_not = state.op.selected() == "NOT";
        let members = inputs.iter().filter(|s| s.is_variadic_member()).count();
        // NOT is single-input: once one member is connected, stop offering
        // the placeholder.
        for socket in inputs.iter_mut() {
            if socket.is_variadic_placeholder() {
                socket.visible = !(is_not && members >= 1);
            }
        }
        state.note = (is_not && members > 1)
            .then(|| "NOT uses input 1 only; disconnect the others".to_owned());
    }

    fn badge(state: &Self::State) -> Option<NodeBadge> {
        state.note.as_ref().map(NodeBadge::warning)
    }
}

// ── Counter (§4.6) ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterState {
    pub start: IntValue,
    pub step: IntValue,
}

pub struct Counter;
impl NodeDef for Counter {
    type State = CounterState;

    fn name() -> &'static str {
        "Counter"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![InputDef::new::<Trigger>("Trigger")]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Number>("Count")]
    }

    fn state() -> Self::State {
        CounterState {
            start: IntValue::plain(0),
            step: IntValue::plain(1),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("start", "Start", |state| &mut state.start),
                PropDef::control("step", "Step", |state| &mut state.step),
            ],
        )]
    }
}

// ── String Formatter (§4.7) ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringFormatterState {
    pub template: StringValue,
}

pub struct StringFormatter;
impl NodeDef for StringFormatter {
    type State = StringFormatterState;

    fn name() -> &'static str {
        "String Formatter"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        // Additional values appear in the template as {1}, {2}, … ({0} and
        // the legacy {n} are the first input).
        vec![InputDef::new::<Number>("Value").variadic(4)]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Text>("Text")]
    }

    fn state() -> Self::State {
        StringFormatterState {
            template: StringValue::new("output/capture_{n:04}.bin"),
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![PropDef::control("template", "Template", |state| {
            &mut state.template
        })]
    }
}

// ── File Writer (§4.8) ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWriterState {
    pub write_width: EnumValue,
    pub index_csv: BoolValue,
}

pub struct FileWriter;
impl NodeDef for FileWriter {
    type State = FileWriterState;

    fn name() -> &'static str {
        "File Writer"
    }
    fn category() -> &'static str {
        "Output"
    }
    fn color() -> Color32 {
        COLOR_OUTPUT
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Words>("Data"),
            InputDef::new::<Text>("Filename"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![]
    }

    fn state() -> Self::State {
        FileWriterState {
            write_width: EnumValue::new(0, &["U8 (low byte)", "U16 LE", "U32 LE"]),
            index_csv: BoolValue::new(true),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("write_width", "Write", |state| &mut state.write_width),
                PropDef::control("index_csv", "Index CSV", |state| &mut state.index_csv),
            ],
        )]
    }
}

// ── TGCK Recorder (§7 Phase 6) ───────────────────────────────────────────────

pub struct TgckRecorder;
impl NodeDef for TgckRecorder {
    type State = ();

    fn name() -> &'static str {
        "TGCK Recorder"
    }
    fn category() -> &'static str {
        "Output"
    }
    fn color() -> Color32 {
        COLOR_OUTPUT
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Words>("Words"),
            InputDef::new::<Signal>("TGCK"),
            InputDef::new::<Text>("Filename"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![]
    }

    fn state() -> Self::State {}
}

// ── Viewer (§4.9) ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewerState {
    pub label: StringValue,
}

pub struct Viewer;
impl NodeDef for Viewer {
    type State = ViewerState;

    fn name() -> &'static str {
        "Viewer"
    }
    fn category() -> &'static str {
        "Output"
    }
    fn color() -> Color32 {
        COLOR_OUTPUT
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        // A lane renders whatever it is fed: raw/derived signals as
        // waveforms, words as annotation boxes, triggers as markers.
        vec![
            InputDef::new::<Signal>("In")
                .accepts::<Words>()
                .accepts::<Trigger>()
                .variadic(16),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![]
    }

    fn state() -> Self::State {
        ViewerState {
            label: StringValue::new(""),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![PropDef::control("label", "Label", |state| &mut state.label)],
        )]
    }
}

// ── Registry ─────────────────────────────────────────────────────────────────

pub fn build_registry() -> NodeTypeRegistry {
    let mut registry = NodeTypeRegistry::new();
    registry.register::<DslFileSource>();
    registry.register::<DsLogicU3Pro16>();
    registry.register::<SpiDecoder>();
    registry.register::<UartDecoder>();
    registry.register::<I2cDecoder>();
    registry.register::<BinaryDecoder>();
    registry.register::<WordMatcher>();
    registry.register::<SrFlipFlop>();
    registry.register::<LogicGate>();
    registry.register::<Counter>();
    registry.register::<StringFormatter>();
    registry.register::<FileWriter>();
    registry.register::<TgckRecorder>();
    registry.register::<Viewer>();
    registry
}

// ── Startup graph: the CCD capture pipeline (§1) ─────────────────────────────

fn output_index(widget: &NodeGraphWidget, node: node_graph::NodeId, name: &str) -> usize {
    widget.graph().nodes[&node]
        .outputs
        .iter()
        .position(|socket| socket.name == name)
        .unwrap_or_else(|| panic!("no output socket '{name}'"))
}

fn input_index(widget: &NodeGraphWidget, node: node_graph::NodeId, name: &str) -> usize {
    widget.graph().nodes[&node]
        .inputs
        .iter()
        .position(|socket| socket.name == name && socket.visible)
        .unwrap_or_else(|| panic!("no input socket '{name}'"))
}

fn connect(
    widget: &mut NodeGraphWidget,
    from: (node_graph::NodeId, &str),
    to: (node_graph::NodeId, &str),
) {
    let from_socket = SocketId {
        node: from.0,
        index: output_index(widget, from.0, from.1),
        direction: SocketDirection::Output,
    };
    let to_socket = SocketId {
        node: to.0,
        index: input_index(widget, to.0, to.1),
        direction: SocketDirection::Input,
    };
    widget.graph_mut().add_connection(from_socket, to_socket);
}

/// Builds the CCD analysis pipeline of `ANALYSIS_PIPELINE_DESIGN.md` §1 as
/// the startup graph, wired for `_captures/wipneus5.dsl` (SPI cs=8 clk=7
/// mosi=6; parallel strobe=10 (ACDK), data D0..D7 = ch 0..7).
///
/// The enable gate is `AND(CS, Q)` with no NOT node: CS idles high and the
/// parallel bus is decodable only while it is *inactive* (channels 6/7 are
/// multiplexed with SPI), so the raw active-low line already is the
/// "inverted SPI enable" of the requirement.
pub fn populate_startup(widget: &mut NodeGraphWidget) {
    use egui::Pos2;

    let add = |widget: &mut NodeGraphWidget, name: &str, x: f32, y: f32| {
        widget
            .add_node_at(name, Pos2::new(x, y))
            .unwrap_or_else(|| panic!("unknown node type '{name}'"))
    };

    let source = add(widget, DslFileSource::name(), 40.0, 260.0);
    let spi = add(widget, SpiDecoder::name(), 330.0, 120.0);
    let start = add(widget, WordMatcher::name(), 620.0, 40.0);
    let stop = add(widget, WordMatcher::name(), 620.0, 230.0);
    let counter = add(widget, Counter::name(), 900.0, 40.0);
    let latch = add(widget, SrFlipFlop::name(), 900.0, 230.0);
    let formatter = add(widget, StringFormatter::name(), 1160.0, 40.0);
    let gate = add(widget, LogicGate::name(), 1160.0, 400.0);
    let decoder = add(widget, BinaryDecoder::name(), 1440.0, 260.0);
    let writer = add(widget, FileWriter::name(), 1760.0, 120.0);
    let viewer = add(widget, Viewer::name(), 1760.0, 420.0);

    // Configure states before wiring so `on_update`-driven socket visibility
    // (e.g. hidden MISO) is settled.
    widget.set_node_state(
        spi,
        serde_json::to_value(SpiDecoderState {
            word_size: IntValue::new(24, 1, 32),
            cpol: EnumValue::new(0, &["0", "1"]),
            cpha: EnumValue::new(0, &["0", "1"]),
            bit_order: EnumValue::new(0, &["MSB first", "LSB first"]),
            cs_polarity: EnumValue::new(0, &["Active low", "Active high", "Disabled"]),
            has_miso: BoolValue::new(false),
        })
        .unwrap(),
    );
    let matcher_state = |pattern: &str| {
        serde_json::to_value(WordMatcherState {
            pattern: StringValue::new(pattern),
            mask: StringValue::new("0xFFFFFF"),
            op: default_match_op(),
            field: EnumValue::new(0, &["MOSI", "MISO"]),
            pulse_output: BoolValue::new(false),
        })
        .unwrap()
    };
    widget.set_node_state(start, matcher_state("0x600081"));
    widget.set_node_state(stop, matcher_state("0x600000"));
    let mut decoder_state = BinaryDecoderState {
        sample_on: EnumValue::new(
            0,
            &[
                "Rising (SDR)",
                "Falling (SDR)",
                "Both (DDR)",
                "High level",
                "Low level",
            ],
        ),
        word_size: IntValue::new(1, 1, 8),
        endianness: EnumValue::new(0, &["Little", "Big"]),
        cs_polarity: EnumValue::new(0, &["Disabled", "Active low", "Active high"]),
    };
    decoder_state.sample_on.select("Both (DDR)");
    widget.set_node_state(decoder, serde_json::to_value(decoder_state).unwrap());

    // Descriptive titles (the def is still identified by `type_name`).
    for (id, title) in [(start, "Match Start"), (stop, "Match Stop"), (gate, "Enable Gate")] {
        if let Some(node) = widget.graph_mut().nodes.get_mut(&id) {
            node.title = title.to_owned();
        }
    }

    // SPI control path.
    connect(widget, (source, "Ch 7"), (spi, "CLK"));
    connect(widget, (source, "Ch 6"), (spi, "MOSI"));
    connect(widget, (source, "Ch 8"), (spi, "CS#"));
    connect(widget, (spi, "MOSI Words"), (start, "Words"));
    connect(widget, (spi, "MOSI Words"), (stop, "Words"));
    connect(widget, (start, "Match"), (latch, "Set"));
    connect(widget, (stop, "Match"), (latch, "Reset"));

    // Filename path.
    connect(widget, (start, "Match"), (counter, "Trigger"));
    connect(widget, (counter, "Count"), (formatter, "Value"));
    connect(widget, (formatter, "Text"), (writer, "Filename"));

    // Enable gate: stream window (Q) AND bus free (CS inactive-high).
    connect(widget, (source, "Ch 8"), (gate, "In"));
    connect(widget, (latch, "Q"), (gate, "In"));
    connect(widget, (gate, "Out"), (decoder, "Enable"));

    // Data path.
    connect(widget, (source, "Ch 10"), (decoder, "Clock"));
    for bit in 0..8 {
        connect(widget, (source, &format!("Ch {bit}")), (decoder, "D"));
    }
    connect(widget, (decoder, "Words"), (writer, "Data"));

    // Viewer lanes: flip-flop state, enable, both triggers, decoded words.
    connect(widget, (latch, "Q"), (viewer, "In"));
    connect(widget, (gate, "Out"), (viewer, "In"));
    connect(widget, (start, "Match"), (viewer, "In"));
    connect(widget, (stop, "Match"), (viewer, "In"));
    connect(widget, (decoder, "Words"), (viewer, "In"));
}

/// Path of the first DSL File Source node with a non-empty file, for the
/// logic-analyzer view.
pub fn dsl_file_source_path(graph: &node_graph::GraphState) -> Option<String> {
    graph
        .nodes
        .values()
        .filter(|node| node.def_name() == DslFileSource::name())
        .filter_map(|node| serde_json::from_value::<DslFileSourceState>(node.state.clone()).ok())
        .map(|state| state.file.value)
        .find(|path| !path.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_graph_builds_with_compatible_wiring() {
        let mut widget = NodeGraphWidget::new(build_registry());
        populate_startup(&mut widget);
        let graph = widget.graph();

        assert_eq!(graph.nodes.len(), 11);
        // src→spi 3, spi→matchers 2, matchers→latch 2, start→counter 1,
        // counter→formatter→writer 2, gate ins 2, gate→decoder 1,
        // clock 1, D0–D7 8, decoder→writer 1, viewer lanes 5.
        assert_eq!(graph.connections.len(), 28);

        for connection in &graph.connections {
            let from = &graph.nodes[&connection.from.node].outputs[connection.from.index];
            let to = &graph.nodes[&connection.to.node].inputs[connection.to.index];
            assert!(
                to.accepts(from.effective_type()),
                "incompatible wire {} ({}) -> {} ({})",
                from.name,
                from.effective_type(),
                to.name,
                to.type_name,
            );
        }
    }

    /// Save/load round-trip (§7 Phase 6): serializing the graph to JSON and
    /// restoring it through the registry (the Ctrl+S/Ctrl+O path) must
    /// compile to the same pipeline.
    #[test]
    fn graph_json_round_trip_compiles_identically() {
        use crate::compile::{BuilderRegistry, lower};

        let mut widget = NodeGraphWidget::new(build_registry());
        populate_startup(&mut widget);
        let registry = BuilderRegistry::standard();
        let original = lower(widget.graph(), &registry).expect("original lowers");

        let json = serde_json::to_string(widget.graph()).expect("graph serializes");
        let restored_state: node_graph::GraphState =
            serde_json::from_str(&json).expect("graph deserializes");
        let mut restored = NodeGraphWidget::new(build_registry());
        restored.set_graph(restored_state);

        let reloaded = lower(restored.graph(), &registry).expect("restored lowers");

        assert_eq!(original.nodes.len(), reloaded.nodes.len());
        for (a, b) in original.nodes.iter().zip(&reloaded.nodes) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.builder, b.builder);
            assert_eq!(a.state, b.state, "state of {} changed in round-trip", a.builder);
        }
        let edges = |compiled: &crate::compile::CompiledGraph| {
            let mut edges: Vec<String> = compiled
                .edges
                .iter()
                .map(|edge| {
                    format!(
                        "n{}:{} -> n{}:{} ({})",
                        edge.from.0.0, edge.from.1, edge.to.0.0, edge.to.1, edge.buffer
                    )
                })
                .collect();
            edges.sort();
            edges
        };
        assert_eq!(edges(&original), edges(&reloaded));
    }

    #[test]
    fn dsl_source_path_found_by_def_name_after_rename() {
        let mut widget = NodeGraphWidget::new(build_registry());
        populate_startup(&mut widget);
        let source_id = *widget
            .graph()
            .nodes
            .iter()
            .find(|(_, node)| node.def_name() == DslFileSource::name())
            .map(|(id, _)| id)
            .unwrap();
        widget.graph_mut().nodes.get_mut(&source_id).unwrap().title = "My capture".to_owned();
        assert_eq!(
            dsl_file_source_path(widget.graph()).as_deref(),
            Some("_captures/wipneus5.dsl")
        );
    }
}
