//! Generic vertically stacked panel layout for egui applications.
//!
//! Panel identifiers are opaque strings. The manager owns screen allocation,
//! split dragging, panel visibility and maximize/restore behavior; hosts add
//! arbitrary title-bar and body widgets through [`PanelSlot`].

use std::collections::HashMap;

use egui::{Color32, CornerRadius, CursorIcon, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy)]
pub struct PanelSpec<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub minimum_height: f32,
}

impl<'a> PanelSpec<'a> {
    pub const fn new(id: &'a str, title: &'a str, minimum_height: f32) -> Self {
        Self {
            id,
            title,
            minimum_height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelSlot<'a> {
    TitleBar(&'a str),
    Body(&'a str),
}

#[derive(Debug, Clone)]
pub struct PanelGeometry {
    pub id: String,
    pub title_rect: Rect,
    pub body_rect: Rect,
    pub panel_rect: Rect,
    pub minimized: bool,
    pub maximized: bool,
}

#[derive(Debug, Clone)]
pub struct PanelLayoutResponse {
    pub panels: Vec<PanelGeometry>,
    pub footer_rect: Rect,
}

impl PanelLayoutResponse {
    pub fn panel(&self, id: &str) -> Option<&PanelGeometry> {
        self.panels.iter().find(|panel| panel.id == id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelState {
    pub id: String,
    pub weight: f32,
    pub minimized: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerticalPanelLayoutState {
    #[serde(default)]
    pub panels: Vec<PanelState>,
    #[serde(default)]
    pub maximized: Option<String>,
    #[serde(default)]
    restore_minimized: Vec<(String, bool)>,
}

#[derive(Debug, Clone)]
pub struct PanelLayoutStyle {
    pub title_height: f32,
    pub splitter_height: f32,
    pub splitter_visual_height: f32,
    pub horizontal_margin: f32,
    pub corner_radius: u8,
    pub title_fill: Color32,
    pub title_hover_fill: Color32,
    pub panel_fill: Color32,
    pub border_color: Color32,
    pub splitter_fill: Color32,
    pub splitter_drag_fill: Color32,
}

impl Default for PanelLayoutStyle {
    fn default() -> Self {
        Self {
            title_height: 28.0,
            splitter_height: 4.0,
            splitter_visual_height: 2.0,
            horizontal_margin: 4.0,
            corner_radius: 7,
            title_fill: Color32::from_rgb(38, 38, 38),
            title_hover_fill: Color32::from_rgb(47, 47, 47),
            panel_fill: Color32::from_rgb(28, 28, 28),
            border_color: Color32::from_rgb(78, 78, 78),
            splitter_fill: Color32::from_rgb(16, 16, 16),
            splitter_drag_fill: Color32::from_rgb(90, 90, 90),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VerticalPanelLayout {
    state: VerticalPanelLayoutState,
    style: PanelLayoutStyle,
}

impl Default for VerticalPanelLayout {
    fn default() -> Self {
        Self {
            state: VerticalPanelLayoutState::default(),
            style: PanelLayoutStyle::default(),
        }
    }
}

impl VerticalPanelLayout {
    pub fn new(panels: impl IntoIterator<Item = (impl Into<String>, f32)>) -> Self {
        let states = panels
            .into_iter()
            .map(|(id, weight)| PanelState {
                id: id.into(),
                weight: weight.max(0.001),
                minimized: false,
            })
            .collect();
        Self {
            state: VerticalPanelLayoutState {
                panels: states,
                ..Default::default()
            },
            style: PanelLayoutStyle::default(),
        }
    }

    pub fn from_state(state: VerticalPanelLayoutState) -> Self {
        Self {
            state,
            style: PanelLayoutStyle::default(),
        }
    }

    pub fn state(&self) -> &VerticalPanelLayoutState {
        &self.state
    }

    pub fn set_style(&mut self, style: PanelLayoutStyle) {
        self.style = style;
    }

    pub fn weight(&self, id: &str) -> Option<f32> {
        self.state
            .panels
            .iter()
            .find(|panel| panel.id == id)
            .map(|panel| panel.weight)
    }

    pub fn split_fraction(&self, first: &str, second: &str) -> Option<f32> {
        let first = self.weight(first)?;
        let second = self.weight(second)?;
        Some(first / (first + second).max(0.001))
    }

    pub fn is_minimized(&self, id: &str) -> bool {
        self.state
            .panels
            .iter()
            .find(|panel| panel.id == id)
            .is_none_or(|panel| self.effectively_minimized(panel))
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        footer_height: f32,
        specs: &[PanelSpec<'_>],
        mut add_widget: impl FnMut(PanelSlot<'_>, &mut Ui),
    ) -> PanelLayoutResponse {
        self.synchronize(specs);
        let panel_rect = Rect::from_min_max(
            rect.min,
            egui::pos2(
                rect.right(),
                (rect.bottom() - footer_height).max(rect.top()),
            ),
        );
        let footer_rect = Rect::from_min_max(panel_rect.left_bottom(), rect.right_bottom());

        let mut geometries = self.geometries(panel_rect, specs);
        self.handle_splitters(ui, panel_rect, specs, &mut geometries);

        let mut pending_action = None;
        for (spec, geometry) in specs.iter().zip(&geometries) {
            let action = self.show_title_bar(ui, spec, geometry, &mut add_widget);
            if action.is_some() {
                pending_action = action;
            }
            if !geometry.minimized {
                let mut body_ui = ui.new_child(
                    UiBuilder::new()
                        .id_salt(("panel-body", spec.id))
                        .max_rect(geometry.body_rect)
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                );
                body_ui.set_clip_rect(geometry.body_rect);
                add_widget(PanelSlot::Body(spec.id), &mut body_ui);
            }
            self.finish_panel(ui, geometry);
        }
        if let Some((id, action)) = pending_action {
            self.apply_action(&id, action);
        }

        PanelLayoutResponse {
            panels: geometries,
            footer_rect,
        }
    }

    fn synchronize(&mut self, specs: &[PanelSpec<'_>]) {
        let old: HashMap<_, _> = self
            .state
            .panels
            .drain(..)
            .map(|state| (state.id.clone(), state))
            .collect();
        self.state.panels = specs
            .iter()
            .map(|spec| {
                old.get(spec.id).cloned().unwrap_or_else(|| PanelState {
                    id: spec.id.to_owned(),
                    weight: 1.0,
                    minimized: false,
                })
            })
            .collect();
        if self
            .state
            .maximized
            .as_ref()
            .is_some_and(|id| !specs.iter().any(|spec| spec.id == id))
        {
            self.state.maximized = None;
            self.state.restore_minimized.clear();
        }
    }

    fn geometries(&self, rect: Rect, specs: &[PanelSpec<'_>]) -> Vec<PanelGeometry> {
        let style = &self.style;
        let count = specs.len();
        let width = (rect.width() - style.horizontal_margin * 2.0).max(0.0);
        let body_space = (rect.height()
            - style.title_height * count as f32
            - style.splitter_height * count.saturating_sub(1) as f32)
            .max(0.0);
        let heights = allocate_heights(
            body_space,
            specs,
            &self.state.panels,
            self.state.maximized.as_deref(),
        );
        let mut y = rect.top();
        specs
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                let title_rect = Rect::from_min_size(
                    egui::pos2(rect.left() + style.horizontal_margin, y),
                    egui::vec2(width, style.title_height),
                );
                let minimized = heights[index] <= 0.0;
                let panel_rect = Rect::from_min_size(
                    title_rect.min,
                    egui::vec2(width, style.title_height + heights[index]),
                );
                let body_rect = Rect::from_min_size(
                    title_rect.left_bottom(),
                    egui::vec2(
                        width,
                        (heights[index] - f32::from(style.corner_radius)).max(0.0),
                    ),
                );
                y = panel_rect.bottom();
                if index + 1 < count {
                    y += style.splitter_height;
                }
                PanelGeometry {
                    id: spec.id.to_owned(),
                    title_rect,
                    body_rect,
                    panel_rect,
                    minimized,
                    maximized: self.state.maximized.as_deref() == Some(spec.id),
                }
            })
            .collect()
    }

    fn handle_splitters(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        specs: &[PanelSpec<'_>],
        geometries: &mut Vec<PanelGeometry>,
    ) {
        for index in 0..specs.len().saturating_sub(1) {
            let splitter_rect = Rect::from_min_size(
                geometries[index].panel_rect.left_bottom(),
                egui::vec2(
                    geometries[index].panel_rect.width(),
                    self.style.splitter_height,
                ),
            );
            let draggable = !geometries[index].minimized && !geometries[index + 1].minimized;
            let response = ui.interact(
                splitter_rect,
                ui.id().with(("panel-splitter", index)),
                if draggable {
                    Sense::click_and_drag()
                } else {
                    Sense::hover()
                },
            );
            if response.hovered() && draggable {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            }
            if response.dragged() && draggable {
                let delta = ui.input(|input| input.pointer.delta().y);
                let upper = geometries[index].panel_rect.height() - self.style.title_height;
                let lower = geometries[index + 1].panel_rect.height() - self.style.title_height;
                let pair_height = upper + lower;
                let new_upper = (upper + delta).clamp(
                    specs[index].minimum_height.min(pair_height),
                    (pair_height - specs[index + 1].minimum_height).max(0.0),
                );
                let pair_weight =
                    self.state.panels[index].weight + self.state.panels[index + 1].weight;
                if pair_height > 0.0 {
                    self.state.panels[index].weight = pair_weight * new_upper / pair_height;
                    self.state.panels[index + 1].weight =
                        pair_weight * (pair_height - new_upper) / pair_height;
                    *geometries = self.geometries(rect, specs);
                }
            }
            ui.painter()
                .rect_filled(splitter_rect, 0.0, self.style.splitter_fill);
            if response.dragged() {
                let visual = Rect::from_center_size(
                    splitter_rect.center(),
                    egui::vec2(splitter_rect.width(), self.style.splitter_visual_height),
                );
                ui.painter()
                    .rect_filled(visual, 0.0, self.style.splitter_drag_fill);
            }
        }
    }

    fn show_title_bar(
        &self,
        ui: &mut Ui,
        spec: &PanelSpec<'_>,
        geometry: &PanelGeometry,
        add_widget: &mut impl FnMut(PanelSlot<'_>, &mut Ui),
    ) -> Option<(String, PanelAction)> {
        let response = ui.interact(
            geometry.title_rect,
            ui.id().with(("panel-title", spec.id)),
            Sense::click(),
        );
        let rounding = if geometry.minimized {
            CornerRadius::same(self.style.corner_radius)
        } else {
            CornerRadius {
                nw: self.style.corner_radius,
                ne: self.style.corner_radius,
                sw: 0,
                se: 0,
            }
        };
        ui.painter().rect_filled(
            geometry.title_rect,
            rounding,
            if response.hovered() {
                self.style.title_hover_fill
            } else {
                self.style.title_fill
            },
        );
        ui.painter().line_segment(
            [
                geometry.title_rect.left_bottom(),
                geometry.title_rect.right_bottom(),
            ],
            Stroke::new(1.0, self.style.border_color),
        );

        let mut action = response.double_clicked().then_some(if geometry.maximized {
            PanelAction::RestoreMaximized
        } else {
            PanelAction::Maximize
        });
        let mut title_ui = ui.new_child(
            UiBuilder::new()
                .id_salt(("panel-title-content", spec.id))
                .max_rect(geometry.title_rect.shrink2(egui::vec2(6.0, 2.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        title_ui.label(egui::RichText::new(spec.title).strong());
        add_widget(PanelSlot::TitleBar(spec.id), &mut title_ui);
        title_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let visibility_icon = if geometry.minimized {
                PanelControlIcon::Reveal
            } else {
                PanelControlIcon::Minimize
            };
            let visibility_tooltip = if geometry.minimized {
                "Restore panel"
            } else {
                "Minimize panel"
            };
            if panel_control_button(ui, visibility_icon, visibility_tooltip).clicked() {
                action = Some(if geometry.minimized {
                    PanelAction::RestorePanel
                } else {
                    PanelAction::Minimize
                });
            }
            let maximize_icon = if geometry.maximized {
                PanelControlIcon::RestoreLayout
            } else {
                PanelControlIcon::Maximize
            };
            let maximize_tooltip = if geometry.maximized {
                "Restore panel layout"
            } else {
                "Maximize panel"
            };
            if panel_control_button(ui, maximize_icon, maximize_tooltip).clicked() {
                action = Some(if geometry.maximized {
                    PanelAction::RestoreMaximized
                } else {
                    PanelAction::Maximize
                });
            }
        });
        action.map(|action| (spec.id.to_owned(), action))
    }

    fn finish_panel(&self, ui: &Ui, geometry: &PanelGeometry) {
        let rounding = CornerRadius::same(self.style.corner_radius);
        if !geometry.minimized && geometry.panel_rect.height() > f32::from(self.style.corner_radius)
        {
            let radius = f32::from(self.style.corner_radius);
            let bottom_cap = Rect::from_min_max(
                egui::pos2(
                    geometry.panel_rect.left(),
                    geometry.panel_rect.bottom() - radius,
                ),
                geometry.panel_rect.right_bottom(),
            );
            ui.painter().rect_filled(
                bottom_cap,
                CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: self.style.corner_radius,
                    se: self.style.corner_radius,
                },
                self.style.panel_fill,
            );
        }
        ui.painter().rect_stroke(
            geometry.panel_rect,
            rounding,
            Stroke::new(1.0, self.style.border_color),
            StrokeKind::Inside,
        );
    }

    fn apply_action(&mut self, id: &str, action: PanelAction) {
        match action {
            PanelAction::Minimize => {
                if self.state.maximized.is_some() {
                    self.restore_maximized();
                }
                if let Some(panel) = self.state.panels.iter_mut().find(|panel| panel.id == id) {
                    panel.minimized = true;
                }
                if self.state.panels.iter().all(|panel| panel.minimized)
                    && let Some(panel) = self.state.panels.iter_mut().find(|panel| panel.id != id)
                {
                    panel.minimized = false;
                }
            }
            PanelAction::RestorePanel => {
                if self.state.maximized.is_some() {
                    self.restore_maximized();
                }
                if let Some(panel) = self.state.panels.iter_mut().find(|panel| panel.id == id) {
                    panel.minimized = false;
                }
            }
            PanelAction::Maximize => {
                if self.state.maximized.is_some() {
                    self.restore_maximized();
                }
                self.state.restore_minimized = self
                    .state
                    .panels
                    .iter()
                    .map(|panel| (panel.id.clone(), panel.minimized))
                    .collect();
                self.state.maximized = Some(id.to_owned());
                if let Some(panel) = self.state.panels.iter_mut().find(|panel| panel.id == id) {
                    panel.minimized = false;
                }
            }
            PanelAction::RestoreMaximized => self.restore_maximized(),
        }
    }

    fn restore_maximized(&mut self) {
        let restore: HashMap<_, _> = self.state.restore_minimized.drain(..).collect();
        for panel in &mut self.state.panels {
            if let Some(minimized) = restore.get(&panel.id) {
                panel.minimized = *minimized;
            }
        }
        self.state.maximized = None;
    }

    fn effectively_minimized(&self, panel: &PanelState) -> bool {
        panel.minimized
            || self
                .state
                .maximized
                .as_deref()
                .is_some_and(|maximized| maximized != panel.id)
    }
}

#[derive(Debug, Clone, Copy)]
enum PanelAction {
    Minimize,
    RestorePanel,
    Maximize,
    RestoreMaximized,
}

#[derive(Debug, Clone, Copy)]
enum PanelControlIcon {
    Minimize,
    Reveal,
    Maximize,
    RestoreLayout,
}

fn panel_control_button(ui: &mut Ui, icon: PanelControlIcon, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, Color32::from_rgb(72, 72, 72));
    }
    let stroke = Stroke::new(1.5, ui.visuals().widgets.style(&response).fg_stroke.color);
    let center = rect.center();
    match icon {
        PanelControlIcon::Minimize => {
            // A downward chevron landing on a tray: panel collapse rather
            // than the conventional desktop-window underscore.
            ui.painter().line_segment(
                [
                    egui::pos2(center.x - 4.0, center.y - 3.0),
                    egui::pos2(center.x, center.y + 1.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x, center.y + 1.0),
                    egui::pos2(center.x + 4.0, center.y - 3.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x - 5.0, center.y + 4.0),
                    egui::pos2(center.x + 5.0, center.y + 4.0),
                ],
                stroke,
            );
        }
        PanelControlIcon::Reveal => {
            // The inverse panel gesture: lift content out of the tray.
            ui.painter().line_segment(
                [
                    egui::pos2(center.x - 4.0, center.y + 1.0),
                    egui::pos2(center.x, center.y - 3.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x, center.y - 3.0),
                    egui::pos2(center.x + 4.0, center.y + 1.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x - 5.0, center.y + 4.0),
                    egui::pos2(center.x + 5.0, center.y + 4.0),
                ],
                stroke,
            );
        }
        PanelControlIcon::Maximize => {
            // Four open corner marks read as expansion without resembling a
            // platform window frame.
            let inner = rect.shrink(5.5);
            let arm = 3.0;
            for (corner, horizontal, vertical) in [
                (inner.left_top(), arm, arm),
                (inner.right_top(), -arm, arm),
                (inner.left_bottom(), arm, -arm),
                (inner.right_bottom(), -arm, -arm),
            ] {
                ui.painter().line_segment(
                    [corner, egui::pos2(corner.x + horizontal, corner.y)],
                    stroke,
                );
                ui.painter()
                    .line_segment([corner, egui::pos2(corner.x, corner.y + vertical)], stroke);
            }
        }
        PanelControlIcon::RestoreLayout => {
            // Vertical arrows converge on a split line, describing a return
            // from one expanded panel to the multi-panel layout.
            ui.painter().line_segment(
                [
                    egui::pos2(center.x - 4.0, center.y),
                    egui::pos2(center.x + 4.0, center.y),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x, center.y - 5.0),
                    egui::pos2(center.x, center.y - 1.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x - 2.0, center.y - 3.0),
                    egui::pos2(center.x, center.y - 1.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x + 2.0, center.y - 3.0),
                    egui::pos2(center.x, center.y - 1.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x, center.y + 5.0),
                    egui::pos2(center.x, center.y + 1.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x - 2.0, center.y + 3.0),
                    egui::pos2(center.x, center.y + 1.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x + 2.0, center.y + 3.0),
                    egui::pos2(center.x, center.y + 1.0),
                ],
                stroke,
            );
        }
    }
    response.on_hover_text(tooltip)
}

fn allocate_heights(
    total: f32,
    specs: &[PanelSpec<'_>],
    states: &[PanelState],
    maximized: Option<&str>,
) -> Vec<f32> {
    let mut result = vec![0.0; specs.len()];
    let visible: Vec<_> = states
        .iter()
        .enumerate()
        .filter(|(_, state)| {
            !state.minimized && maximized.is_none_or(|maximized| maximized == state.id)
        })
        .map(|(index, _)| index)
        .collect();
    if visible.is_empty() || total <= 0.0 {
        return result;
    }
    let weight_sum: f32 = visible.iter().map(|index| states[*index].weight).sum();
    for index in &visible {
        result[*index] = total * states[*index].weight / weight_sum.max(0.001);
    }
    if total
        >= visible
            .iter()
            .map(|index| specs[*index].minimum_height)
            .sum()
    {
        let mut fixed = vec![false; specs.len()];
        loop {
            let newly_fixed: Vec<_> = visible
                .iter()
                .copied()
                .filter(|index| !fixed[*index] && result[*index] < specs[*index].minimum_height)
                .collect();
            if newly_fixed.is_empty() {
                break;
            }
            for index in newly_fixed {
                fixed[index] = true;
                result[index] = specs[index].minimum_height;
            }
            let fixed_total: f32 = visible
                .iter()
                .filter(|index| fixed[**index])
                .map(|index| result[*index])
                .sum();
            let free_weight: f32 = visible
                .iter()
                .filter(|index| !fixed[**index])
                .map(|index| states[*index].weight)
                .sum();
            for index in visible.iter().filter(|index| !fixed[**index]) {
                result[*index] =
                    (total - fixed_total) * states[*index].weight / free_weight.max(0.001);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_respects_weights_and_minimums() {
        let specs = [
            PanelSpec::new("top", "Top", 160.0),
            PanelSpec::new("bottom", "Bottom", 160.0),
        ];
        let states = [
            PanelState {
                id: "top".to_owned(),
                weight: 0.25,
                minimized: false,
            },
            PanelState {
                id: "bottom".to_owned(),
                weight: 0.75,
                minimized: false,
            },
        ];
        assert_eq!(
            allocate_heights(800.0, &specs, &states, None),
            [200.0, 600.0]
        );
        assert_eq!(
            allocate_heights(400.0, &specs, &states, None),
            [160.0, 240.0]
        );
    }

    #[test]
    fn maximized_panel_gets_all_body_space() {
        let specs = [
            PanelSpec::new("top", "Top", 100.0),
            PanelSpec::new("bottom", "Bottom", 100.0),
        ];
        let states = [
            PanelState {
                id: "top".to_owned(),
                weight: 1.0,
                minimized: false,
            },
            PanelState {
                id: "bottom".to_owned(),
                weight: 1.0,
                minimized: false,
            },
        ];
        assert_eq!(
            allocate_heights(500.0, &specs, &states, Some("bottom")),
            [0.0, 500.0]
        );
    }

    #[test]
    fn maximizing_and_restoring_preserves_previous_visibility() {
        let mut layout = VerticalPanelLayout::new([("top", 1.0), ("bottom", 1.0)]);
        layout.apply_action("top", PanelAction::Minimize);
        layout.apply_action("bottom", PanelAction::Maximize);
        assert!(layout.is_minimized("top"));
        assert!(!layout.is_minimized("bottom"));

        layout.apply_action("bottom", PanelAction::RestoreMaximized);
        assert!(layout.is_minimized("top"));
        assert!(!layout.is_minimized("bottom"));
    }

    #[test]
    fn restoring_other_panel_exits_maximize_without_inverting_visibility() {
        let mut layout = VerticalPanelLayout::new([("top", 1.0), ("bottom", 1.0)]);
        layout.apply_action("top", PanelAction::Maximize);
        layout.apply_action("bottom", PanelAction::RestorePanel);
        assert!(!layout.is_minimized("top"));
        assert!(!layout.is_minimized("bottom"));
    }
}
