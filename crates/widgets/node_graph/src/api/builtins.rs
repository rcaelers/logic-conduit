use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Sense, Ui, Vec2};
use serde::{Deserialize, Serialize};

use super::control::InlineControl;
use super::socket::{SocketDef, SocketWithControlDef};
use crate::model::SocketShape;

// ── Built-in socket types ─────────────────────────────────────────────────────

pub struct BoolSocket;
pub struct IntSocket;
pub struct FloatSocket;
pub struct StrSocket;
pub struct FileSocket;
/// Wildcard type: accepts (and is accepted by) every other type. Useful as
/// the native type of variadic placeholder inputs and reroute nodes.
pub struct AnySocket;

impl SocketDef for AnySocket {
    type Value = ();

    fn type_name() -> &'static str {
        "Any"
    }
    fn color() -> Color32 {
        Color32::from_rgb(150, 150, 150)
    }
}

// Builtin config sockets follow the graph-wide styling axes:
// square = static config, and the hue is the payload family shared with the
// stream types (green logic, blue integer, violet float, rose text, tan file).
// Red is reserved for error feedback, grey for the wildcard.

impl SocketDef for BoolSocket {
    type Value = bool;

    fn type_name() -> &'static str {
        "Bool"
    }
    fn color() -> Color32 {
        Color32::from_rgb(95, 175, 95)
    }
    fn shape() -> SocketShape {
        SocketShape::Square
    }
}

impl SocketDef for IntSocket {
    type Value = i32;

    fn type_name() -> &'static str {
        "Int"
    }
    fn color() -> Color32 {
        Color32::from_rgb(95, 145, 210)
    }
    fn shape() -> SocketShape {
        SocketShape::Square
    }
}

impl SocketDef for FloatSocket {
    type Value = f32;

    fn type_name() -> &'static str {
        "Float"
    }
    fn color() -> Color32 {
        Color32::from_rgb(165, 130, 215)
    }
    fn shape() -> SocketShape {
        SocketShape::Square
    }
}

impl SocketDef for StrSocket {
    type Value = String;

    fn type_name() -> &'static str {
        "String"
    }
    fn color() -> Color32 {
        Color32::from_rgb(215, 150, 170)
    }
    fn shape() -> SocketShape {
        SocketShape::Square
    }
}

impl SocketDef for FileSocket {
    type Value = String;

    fn type_name() -> &'static str {
        "File"
    }
    fn color() -> Color32 {
        Color32::from_rgb(170, 145, 95)
    }
    fn shape() -> SocketShape {
        SocketShape::Square
    }
}

impl SocketWithControlDef for BoolSocket {
    type Control = BoolValue;
}

impl SocketWithControlDef for IntSocket {
    type Control = IntValue;
}

impl SocketWithControlDef for FloatSocket {
    type Control = FloatValue;
}

impl SocketWithControlDef for StrSocket {
    type Control = StringValue;
}

impl SocketWithControlDef for FileSocket {
    type Control = FileValue;
}

// ── Built-in value types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntValue {
    pub value: i32,
    pub min: i32,
    pub max: i32,
}

impl IntValue {
    pub fn new(value: i32, min: i32, max: i32) -> Self {
        Self { value, min, max }
    }
    pub fn plain(value: i32) -> Self {
        Self {
            value,
            min: i32::MIN,
            max: i32::MAX,
        }
    }
}

impl InlineControl for IntValue {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let drag = if resp.dragged() {
            resp.drag_delta().x
        } else {
            0.0
        };
        let old = self.value;
        if drag.abs() > 0.01 {
            self.value = (self.value as f32 + drag * 0.1).round() as i32;
            if self.min != i32::MIN || self.max != i32::MAX {
                self.value = self.value.clamp(self.min, self.max);
            }
        }
        let fill = if self.max > self.min && self.max != i32::MAX {
            Some((self.value - self.min) as f32 / (self.max - self.min) as f32)
        } else {
            None
        };
        paint_number_btn(
            &ui.painter().with_clip_rect(clip_rect),
            rect,
            label,
            &self.value.to_string(),
            fill,
            zoom,
        );
        self.value != old
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatValue {
    pub value: f32,
    pub min: f32,
    pub max: f32,
    pub speed: f32,
}

impl FloatValue {
    pub fn new(value: f32, min: f32, max: f32, speed: f32) -> Self {
        Self {
            value,
            min,
            max,
            speed,
        }
    }
    pub fn with_range(value: f32, min: f32, max: f32) -> Self {
        let speed = if max > min { (max - min) / 100.0 } else { 0.01 };
        Self {
            value,
            min,
            max,
            speed,
        }
    }
    pub fn plain(value: f32) -> Self {
        Self {
            value,
            min: f32::NEG_INFINITY,
            max: f32::INFINITY,
            speed: 0.01,
        }
    }
}

impl InlineControl for FloatValue {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let drag = if resp.dragged() {
            resp.drag_delta().x
        } else {
            0.0
        };
        let old = self.value.to_bits();
        if drag.abs() > 0.01 {
            self.value += drag * self.speed;
            if self.min.is_finite() && self.max.is_finite() {
                self.value = self.value.clamp(self.min, self.max);
            }
        }
        let fill = if self.min.is_finite() && self.max.is_finite() && self.max > self.min {
            Some((self.value - self.min) / (self.max - self.min))
        } else {
            None
        };
        paint_number_btn(
            &ui.painter().with_clip_rect(clip_rect),
            rect,
            label,
            &fmt_float(self.value),
            fill,
            zoom,
        );
        self.value.to_bits() != old
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoolValue {
    pub value: bool,
}

impl BoolValue {
    pub fn new(value: bool) -> Self {
        Self { value }
    }
}

impl InlineControl for BoolValue {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        let old = self.value;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.style_mut().spacing.item_spacing = Vec2::splat(2.0 * zoom);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.value, label);
                });
            },
        );
        self.value != old
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringValue {
    pub value: String,
}

impl StringValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

impl InlineControl for StringValue {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        let old = self.value.clone();
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.style_mut().spacing.item_spacing = Vec2::splat(2.0 * zoom);
                ui.add(
                    egui::TextEdit::singleline(&mut self.value)
                        .hint_text(label)
                        .desired_width(rect.width() - 4.0 * zoom),
                );
            },
        );
        self.value != old
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileValue {
    pub value: String,
    #[serde(default)]
    pub dialog_title: String,
    #[serde(default)]
    pub filters: Vec<FileFilter>,
    /// Browse with a *save* dialog (pick a new/overwrite target) instead of
    /// an *open* dialog (pick an existing file).
    #[serde(default)]
    pub save: bool,
}

impl FileValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            dialog_title: "Select file".to_string(),
            filters: Vec::new(),
            save: false,
        }
    }

    /// A picker whose browse button opens a save dialog.
    pub fn new_save(value: impl Into<String>, dialog_title: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            dialog_title: dialog_title.into(),
            filters: Vec::new(),
            save: true,
        }
    }

    pub fn with_filter(
        value: impl Into<String>,
        dialog_title: impl Into<String>,
        filter_name: impl Into<String>,
        extensions: &[&str],
    ) -> Self {
        Self {
            value: value.into(),
            dialog_title: dialog_title.into(),
            filters: vec![FileFilter {
                name: filter_name.into(),
                extensions: extensions
                    .iter()
                    .map(|extension| extension.to_string())
                    .collect(),
            }],
            save: false,
        }
    }
}

impl InlineControl for FileValue {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        let old = self.value.clone();
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.style_mut().spacing.item_spacing = Vec2::splat(2.0 * zoom);
                let button_width = 28.0 * zoom;
                ui.add(
                    egui::TextEdit::singleline(&mut self.value)
                        .hint_text(label)
                        .desired_width((rect.width() - button_width - 6.0 * zoom).max(24.0 * zoom)),
                );
                if ui
                    .add_enabled(super::file_dialog::AVAILABLE, egui::Button::new("…"))
                    .clicked()
                    && let Some(path) =
                        super::file_dialog::pick(&self.dialog_title, &self.filters, self.save)
                {
                    self.value = path;
                }
            },
        );
        self.value != old
    }
}

#[derive(Debug, Clone)]
pub struct EnumValue {
    pub index: usize,
    pub variants: Vec<String>,
}

impl EnumValue {
    pub fn new(index: usize, variants: &[&str]) -> Self {
        Self {
            index,
            variants: variants.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The currently selected variant name ("" when out of range).
    pub fn selected(&self) -> &str {
        self.variants.get(self.index).map_or("", String::as_str)
    }

    /// Selects `name` if it is a known variant; ignores unknown names.
    pub fn select(&mut self, name: &str) {
        if let Some(index) = self.variants.iter().position(|variant| variant == name) {
            self.index = index;
        }
    }
}

/// Persisted by variant *name*, not index, so saved graphs survive variant
/// reorders in node defs. Legacy files that stored only an index still load.
impl Serialize for EnumValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("EnumValue", 2)?;
        s.serialize_field("value", self.selected())?;
        s.serialize_field("variants", &self.variants)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for EnumValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Repr {
            #[serde(default)]
            value: Option<String>,
            #[serde(default)]
            index: Option<usize>,
            #[serde(default)]
            variants: Vec<String>,
        }
        let repr = Repr::deserialize(deserializer)?;
        let index = repr
            .value
            .and_then(|name| repr.variants.iter().position(|variant| *variant == name))
            .or(repr.index)
            .unwrap_or(0)
            .min(repr.variants.len().saturating_sub(1));
        Ok(Self {
            index,
            variants: repr.variants,
        })
    }
}

impl InlineControl for EnumValue {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        let old = self.index;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.style_mut().spacing.item_spacing = Vec2::splat(2.0 * zoom);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).size(10.0 * zoom));
                    let selected = self.variants.get(self.index).cloned().unwrap_or_default();
                    let vars = self.variants.clone();
                    let mut new_idx = self.index;
                    egui::ComboBox::from_id_salt(egui::Id::new(("enum_val", label)))
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for (vi, variant) in vars.iter().enumerate() {
                                if ui.selectable_label(new_idx == vi, variant).clicked() {
                                    new_idx = vi;
                                }
                            }
                        });
                    self.index = new_idx;
                });
            },
        );
        self.index != old
    }
}

// ── Shared rendering helpers ──────────────────────────────────────────────────

fn fmt_float(v: f32) -> String {
    if v == v.trunc() && v.abs() < 1e6 {
        format!("{:.0}", v)
    } else {
        format!("{:.3}", v)
    }
}

fn paint_number_btn(
    painter: &egui::Painter,
    rect: Rect,
    label: &str,
    value: &str,
    fill_ratio: Option<f32>,
    zoom: f32,
) {
    let rounding = CornerRadius::same(3);
    painter.rect_filled(rect, rounding, Color32::from_rgb(56, 56, 56));
    if let Some(ratio) = fill_ratio {
        let ratio = ratio.clamp(0.0, 1.0);
        if ratio > 0.001 {
            let fill_rect =
                Rect::from_min_size(rect.min, Vec2::new(rect.width() * ratio, rect.height()));
            painter.rect_filled(
                fill_rect,
                rounding,
                Color32::from_rgba_unmultiplied(61, 133, 224, 120),
            );
        }
    }
    let text_color = Color32::from_rgb(210, 210, 210);
    let font = FontId::proportional((11.0 * zoom).clamp(7.0, 14.0));
    painter.text(
        Pos2::new(rect.left() + 5.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font.clone(),
        text_color,
    );
    painter.text(
        Pos2::new(rect.right() - 5.0, rect.center().y),
        Align2::RIGHT_CENTER,
        value,
        font,
        text_color,
    );
}
