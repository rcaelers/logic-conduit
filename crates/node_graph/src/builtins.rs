use crate::types::{SocketShape, SocketTypeDef};
use crate::value::NodeValue;
use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, Sense, Ui, Vec2};
use serde::{Deserialize, Serialize};
use std::any::Any;

// ── Built-in socket types ─────────────────────────────────────────────────────

pub struct BoolSocket;
pub struct IntSocket;
pub struct FloatSocket;
pub struct StrSocket;
pub struct AnySocket;

impl SocketTypeDef for BoolSocket {
    fn type_name() -> &'static str {
        "Bool"
    }
    fn color() -> Color32 {
        Color32::from_rgb(200, 80, 80)
    }
    fn shape() -> SocketShape {
        SocketShape::Square
    }
}

impl SocketTypeDef for IntSocket {
    fn type_name() -> &'static str {
        "Int"
    }
    fn color() -> Color32 {
        Color32::from_rgb(100, 180, 100)
    }
    fn shape() -> SocketShape {
        SocketShape::Diamond
    }
}

impl SocketTypeDef for FloatSocket {
    fn type_name() -> &'static str {
        "Float"
    }
    fn color() -> Color32 {
        Color32::from_rgb(160, 160, 160)
    }
}

impl SocketTypeDef for StrSocket {
    fn type_name() -> &'static str {
        "String"
    }
    fn color() -> Color32 {
        Color32::from_rgb(200, 160, 160)
    }
}

impl SocketTypeDef for AnySocket {
    fn type_name() -> &'static str {
        "Any"
    }
    fn color() -> Color32 {
        Color32::from_rgb(150, 150, 150)
    }
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
    pub fn plain(value: i32) -> Box<dyn NodeValue> {
        Box::new(Self {
            value,
            min: i32::MIN,
            max: i32::MAX,
        })
    }
}

impl From<IntValue> for Box<dyn NodeValue> {
    fn from(v: IntValue) -> Self {
        Box::new(v)
    }
}

#[typetag::serde]
impl NodeValue for IntValue {
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
    fn clone_box(&self) -> Box<dyn NodeValue> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
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
    pub fn with_range(value: f32, min: f32, max: f32) -> Box<dyn NodeValue> {
        let speed = if max > min { (max - min) / 100.0 } else { 0.01 };
        Box::new(Self {
            value,
            min,
            max,
            speed,
        })
    }
    pub fn plain(value: f32) -> Box<dyn NodeValue> {
        Box::new(Self {
            value,
            min: f32::NEG_INFINITY,
            max: f32::INFINITY,
            speed: 0.01,
        })
    }
}

impl From<FloatValue> for Box<dyn NodeValue> {
    fn from(v: FloatValue) -> Self {
        Box::new(v)
    }
}

#[typetag::serde]
impl NodeValue for FloatValue {
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
    fn clone_box(&self) -> Box<dyn NodeValue> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
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

impl From<BoolValue> for Box<dyn NodeValue> {
    fn from(v: BoolValue) -> Self {
        Box::new(v)
    }
}

#[typetag::serde]
impl NodeValue for BoolValue {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        _zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        let old = self.value;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.style_mut().spacing.item_spacing = Vec2::splat(2.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.value, label);
                });
            },
        );
        self.value != old
    }
    fn clone_box(&self) -> Box<dyn NodeValue> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
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

impl From<StringValue> for Box<dyn NodeValue> {
    fn from(v: StringValue) -> Self {
        Box::new(v)
    }
}

#[typetag::serde]
impl NodeValue for StringValue {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        _zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        let old = self.value.clone();
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.style_mut().spacing.item_spacing = Vec2::splat(2.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.value)
                        .hint_text(label)
                        .desired_width(rect.width() - 4.0),
                );
            },
        );
        self.value != old
    }
    fn clone_box(&self) -> Box<dyn NodeValue> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl From<EnumValue> for Box<dyn NodeValue> {
    fn from(v: EnumValue) -> Self {
        Box::new(v)
    }
}

#[typetag::serde]
impl NodeValue for EnumValue {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        _zoom: f32,
        clip_rect: Rect,
    ) -> bool {
        let old = self.index;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::LEFT)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
                ui.style_mut().spacing.item_spacing = Vec2::splat(2.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).size(10.0));
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
    fn clone_box(&self) -> Box<dyn NodeValue> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
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
