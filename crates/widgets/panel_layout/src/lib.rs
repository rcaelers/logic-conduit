//! Generic split-panel layout for egui applications.
//!
//! Panel and content identifiers are opaque strings. The manager owns the
//! split tree, boundary menus, split placement, content selection, dragging,
//! closing, and maximizing. Hosts provide content descriptions and render
//! arbitrary title-bar and body widgets through [`PanelSlot`].

use std::collections::HashSet;

use egui::{
    Color32, CornerRadius, CursorIcon, KeyboardShortcut, Rect, Sense, Stroke, StrokeKind, Ui,
    UiBuilder,
};
use serde::{Deserialize, Serialize};

use input_bindings::MenuShortcut;
use widget_support::menu_item_layout_job;

const BOUNDARY_SNAP_GRID: f32 = 16.0;
const BOUNDARY_ALIGNMENT_DISTANCE: f32 = 10.0;
const BOUNDARY_EXTEND_DISTANCE: f32 = 12.0;

type BoundaryHandling = (
    Vec<LayoutAction>,
    Option<BoundaryInteraction>,
    Option<(SplitAxis, f32)>,
    bool,
);

#[derive(Debug, Clone, Copy)]
pub struct PanelSpec<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub icon: PanelIcon,
    pub minimum_width: f32,
    pub minimum_height: f32,
    pub singleton: bool,
}

impl<'a> PanelSpec<'a> {
    pub const fn new(id: &'a str, title: &'a str, minimum_height: f32) -> Self {
        Self {
            id,
            title,
            icon: PanelIcon::Panel,
            minimum_width: minimum_height,
            minimum_height,
            singleton: false,
        }
    }

    pub const fn minimum_width(mut self, minimum_width: f32) -> Self {
        self.minimum_width = minimum_width;
        self
    }

    pub const fn icon(mut self, icon: PanelIcon) -> Self {
        self.icon = icon;
        self
    }

    pub const fn singleton(mut self) -> Self {
        self.singleton = true;
        self
    }
}

/// Application-neutral vector icons for panel content selectors.
///
/// Hosts explicitly select an icon when declaring a [`PanelSpec`]; the panel
/// manager never derives presentation from content identifiers or titles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PanelIcon {
    #[default]
    Panel,
    Waveform,
    Network,
    List,
    Target,
    Table,
    Reset,
}

impl PanelIcon {
    fn paint(self, ui: &Ui, rect: Rect, color: Color32) {
        let painter = ui.painter();
        let rect = Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0));
        let stroke = Stroke::new(1.5, color);
        match self {
            Self::Panel => {
                let panel = rect.shrink(2.0);
                painter.rect_stroke(panel, 2.0, stroke, StrokeKind::Inside);
                painter.line_segment(
                    [
                        egui::pos2(panel.left(), panel.top() + 4.0),
                        egui::pos2(panel.right(), panel.top() + 4.0),
                    ],
                    stroke,
                );
            }
            Self::Waveform => {
                let left = rect.left() + 1.0;
                let right = rect.right() - 1.0;
                let high = rect.top() + 4.0;
                let low = rect.bottom() - 4.0;
                let quarter = (right - left) * 0.25;
                painter.add(egui::Shape::line(
                    vec![
                        egui::pos2(left, low),
                        egui::pos2(left + quarter, low),
                        egui::pos2(left + quarter, high),
                        egui::pos2(left + quarter * 3.0, high),
                        egui::pos2(left + quarter * 3.0, low),
                        egui::pos2(right, low),
                    ],
                    stroke,
                ));
            }
            Self::Network => {
                let first = egui::pos2(rect.left() + 3.0, rect.center().y);
                let upper = egui::pos2(rect.right() - 3.0, rect.top() + 3.5);
                let lower = egui::pos2(rect.right() - 3.0, rect.bottom() - 3.5);
                painter.line_segment([first, upper], stroke);
                painter.line_segment([first, lower], stroke);
                for center in [first, upper, lower] {
                    painter.circle_filled(center, 2.4, color);
                }
            }
            Self::List => {
                for offset in [-4.0, 0.0, 4.0] {
                    let y = rect.center().y + offset;
                    painter.circle_filled(egui::pos2(rect.left() + 3.0, y), 1.3, color);
                    painter.line_segment(
                        [
                            egui::pos2(rect.left() + 6.0, y),
                            egui::pos2(rect.right() - 1.0, y),
                        ],
                        stroke,
                    );
                }
            }
            Self::Target => {
                let center = rect.center();
                painter.circle_stroke(center, 4.2, stroke);
                painter.circle_filled(center, 1.7, color);
                painter.line_segment(
                    [
                        egui::pos2(center.x, rect.top() + 1.0),
                        egui::pos2(center.x, center.y - 5.5),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        egui::pos2(center.x, center.y + 5.5),
                        egui::pos2(center.x, rect.bottom() - 1.0),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        egui::pos2(rect.left() + 1.0, center.y),
                        egui::pos2(center.x - 5.5, center.y),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        egui::pos2(center.x + 5.5, center.y),
                        egui::pos2(rect.right() - 1.0, center.y),
                    ],
                    stroke,
                );
            }
            Self::Table => {
                let table = rect.shrink(1.5);
                painter.rect_stroke(table, 1.5, stroke, StrokeKind::Inside);
                for fraction in [1.0 / 3.0, 2.0 / 3.0] {
                    let x = egui::lerp(table.left()..=table.right(), fraction);
                    let y = egui::lerp(table.top()..=table.bottom(), fraction);
                    painter.line_segment(
                        [egui::pos2(x, table.top()), egui::pos2(x, table.bottom())],
                        stroke,
                    );
                    painter.line_segment(
                        [egui::pos2(table.left(), y), egui::pos2(table.right(), y)],
                        stroke,
                    );
                }
            }
            Self::Reset => {
                let center = rect.center();
                let radius = 5.5;
                let points = (0..=12)
                    .map(|step| {
                        let angle = -2.5 + step as f32 * 4.5 / 12.0;
                        center + egui::vec2(angle.cos(), angle.sin()) * radius
                    })
                    .collect();
                painter.add(egui::Shape::line(points, stroke));
                let tip = center + egui::vec2((-2.5_f32).cos(), (-2.5_f32).sin()) * radius;
                painter.line_segment([tip, tip + egui::vec2(0.5, -3.5)], stroke);
                painter.line_segment([tip, tip + egui::vec2(3.4, -0.8)], stroke);
            }
        }
    }

    /// Adds an icon-and-label row suitable for an egui popup menu.
    pub fn menu_item(self, ui: &mut Ui, label: &str) -> egui::Response {
        ui.add(PanelIconMenuItem { icon: self, label })
    }
}

struct PanelIconMenuItem<'a> {
    icon: PanelIcon,
    label: &'a str,
}

impl egui::Widget for PanelIconMenuItem<'_> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let width = ui.available_width().max(150.0);
        let response = ui.add_sized(
            [width, 24.0],
            egui::Button::new(egui::RichText::new(self.label).color(Color32::TRANSPARENT)),
        );
        let color = ui.visuals().widgets.style(&response).fg_stroke.color;
        self.icon.paint(
            ui,
            Rect::from_center_size(
                egui::pos2(response.rect.left() + 14.0, response.rect.center().y),
                egui::vec2(16.0, 16.0),
            ),
            color,
        );
        ui.painter().text(
            egui::pos2(response.rect.left() + 28.0, response.rect.center().y),
            egui::Align2::LEFT_CENTER,
            self.label,
            egui::TextStyle::Button.resolve(ui.style()),
            color,
        );
        response
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelSlot<'a> {
    TitleBar {
        panel_id: &'a str,
        content_id: &'a str,
    },
    Body {
        panel_id: &'a str,
        content_id: &'a str,
    },
}

#[derive(Debug, Clone)]
pub struct PanelGeometry {
    pub panel_id: String,
    pub content_id: String,
    pub title_rect: Rect,
    /// Empty title-bar area that accepts area-level mouse gestures. Content
    /// selectors, host-provided text/buttons, and panel controls are excluded.
    pub title_interaction_rect: Option<Rect>,
    pub body_rect: Rect,
    pub panel_rect: Rect,
    pub allocated_rect: Rect,
    pub title_bar_position: TitleBarPosition,
    pub maximized: bool,
}

#[derive(Debug, Clone)]
pub struct PanelLayoutResponse {
    pub panels: Vec<PanelGeometry>,
    pub footer_rect: Rect,
    pub boundary_interaction: Option<BoundaryInteraction>,
    pub boundary_break_available: bool,
}

impl PanelLayoutResponse {
    pub fn panel(&self, panel_id: &str) -> Option<&PanelGeometry> {
        self.panels.iter().find(|panel| panel.panel_id == panel_id)
    }

    pub fn content_panel(&self, content_id: &str) -> Option<&PanelGeometry> {
        self.panels
            .iter()
            .find(|panel| panel.content_id == content_id)
    }
}

/// Pointer interaction currently taking place on a boundary between panels.
///
/// Hosts can use this application-neutral state to select an input-binding
/// context for status hints without teaching the layout manager about those
/// bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryInteraction {
    Hovered,
    Dragging,
    DraggingWithParallelBoundary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelState {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub title_bar_position: TitleBarPosition,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleBarPosition {
    #[default]
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    /// A horizontal boundary with panels above and below it.
    Horizontal,
    /// A vertical boundary with panels to its left and right.
    Vertical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutNode {
    Panel {
        panel: PanelState,
    },
    Split {
        id: u64,
        axis: SplitAxis,
        fraction: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PanelLayoutState {
    #[serde(default)]
    root: Option<LayoutNode>,
    #[serde(default)]
    maximized: Option<String>,
    #[serde(default = "default_next_id")]
    next_id: u64,
}

fn default_next_id() -> u64 {
    1
}

#[derive(Debug, Clone)]
pub struct PanelLayoutStyle {
    pub title_height: f32,
    pub splitter_size: f32,
    pub splitter_visual_size: f32,
    pub outer_margin: f32,
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
            splitter_size: 4.0,
            splitter_visual_size: 2.0,
            outer_margin: 4.0,
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

#[derive(Debug, Clone, Default)]
pub struct PanelLayout {
    state: PanelLayoutState,
    style: PanelLayoutStyle,
    boundary_context: Option<BoundaryContext>,
    split_placement: Option<SplitPlacement>,
    maximize_shortcut: Option<KeyboardShortcut>,
}

impl PanelLayout {
    /// Creates a top-to-bottom layout. The supplied weights determine the
    /// initial horizontal split fractions.
    pub fn new(panels: impl IntoIterator<Item = (impl Into<String>, f32)>) -> Self {
        let panels: Vec<_> = panels
            .into_iter()
            .map(|(content, weight)| (content.into(), weight.max(0.001)))
            .collect();
        let mut next_id = 1;
        let root = build_vertical_tree(&panels, &mut next_id);
        Self {
            state: PanelLayoutState {
                root,
                next_id,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub fn from_state(mut state: PanelLayoutState) -> Self {
        state.next_id = state
            .next_id
            .max(max_layout_id(state.root.as_ref()).saturating_add(1))
            .max(1);
        Self {
            state,
            ..Default::default()
        }
    }

    pub fn state(&self) -> &PanelLayoutState {
        &self.state
    }

    pub fn set_style(&mut self, style: PanelLayoutStyle) {
        self.style = style;
    }

    /// Configures the keyboard shortcut that toggles the area under the
    /// pointer between maximized and restored layout states.
    pub fn set_maximize_shortcut(&mut self, shortcut: Option<KeyboardShortcut>) {
        self.maximize_shortcut = shortcut;
    }

    pub fn split_fraction(&self, first_content: &str, second_content: &str) -> Option<f32> {
        find_content_split_fraction(self.state.root.as_ref()?, first_content, second_content)
    }

    /// Ensures `content_id` is visible in a top-to-bottom column on the right
    /// side of the complete layout.
    ///
    /// If a right column containing only `ordered_contents` already exists,
    /// the new content is inserted there and the column is rebuilt in the
    /// supplied order. Otherwise the complete current layout is wrapped in a
    /// new vertical split. Identifiers and ordering are opaque to the manager.
    pub fn ensure_right_column_content(
        &mut self,
        content_id: &str,
        ordered_contents: &[&str],
        existing_layout_fraction: f32,
    ) -> bool {
        if !ordered_contents.contains(&content_id)
            || find_panel_by_content(self.state.root.as_ref(), content_id).is_some()
        {
            return false;
        }
        self.add_right_column_content(content_id, ordered_contents, existing_layout_fraction)
    }

    /// Ensures that at least `count` panels with `content_id` are present.
    /// Additional instances are placed in the same ordered right-side column.
    pub fn ensure_right_column_content_count(
        &mut self,
        content_id: &str,
        count: usize,
        ordered_contents: &[&str],
        existing_layout_fraction: f32,
    ) -> bool {
        if !ordered_contents.contains(&content_id) {
            return false;
        }
        let existing = all_panels(self.state.root.as_ref())
            .into_iter()
            .filter(|panel| panel.content == content_id)
            .count();
        let mut changed = false;
        for _ in existing..count {
            changed |= self.add_right_column_content(
                content_id,
                ordered_contents,
                existing_layout_fraction,
            );
        }
        changed
    }

    fn add_right_column_content(
        &mut self,
        content_id: &str,
        ordered_contents: &[&str],
        existing_layout_fraction: f32,
    ) -> bool {
        self.restore_maximized();

        let existing_column = match self.state.root.as_ref() {
            Some(LayoutNode::Split {
                axis: SplitAxis::Vertical,
                second,
                ..
            }) if subtree_contains_only(second, ordered_contents) => Some(
                all_panels(Some(second))
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        };
        let new_panel = PanelState {
            id: self.allocate_id("panel"),
            content: content_id.to_owned(),
            title_bar_position: TitleBarPosition::Top,
        };

        if let Some(mut panels) = existing_column {
            panels.push(new_panel);
            panels.sort_by_key(|panel| {
                ordered_contents
                    .iter()
                    .position(|content| *content == panel.content)
                    .unwrap_or(usize::MAX)
            });
            let split_ids: Vec<_> = (1..panels.len())
                .map(|_| self.allocate_numeric_id())
                .collect();
            let column = build_panel_column(&panels, &split_ids);
            let Some(LayoutNode::Split { second, .. }) = self.state.root.as_mut() else {
                return false;
            };
            **second = column;
        } else if let Some(existing) = self.state.root.take() {
            let split_id = self.allocate_numeric_id();
            self.state.root = Some(LayoutNode::Split {
                id: split_id,
                axis: SplitAxis::Vertical,
                fraction: existing_layout_fraction.clamp(0.1, 0.9),
                first: Box::new(existing),
                second: Box::new(LayoutNode::Panel { panel: new_panel }),
            });
        } else {
            self.state.root = Some(LayoutNode::Panel { panel: new_panel });
        }
        true
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
        let layout_rect = Rect::from_min_max(
            rect.min,
            egui::pos2(
                rect.right(),
                (rect.bottom() - footer_height).max(rect.top()),
            ),
        );
        let footer_rect = Rect::from_min_max(layout_rect.left_bottom(), rect.right_bottom());
        let root_rect = layout_rect.shrink2(egui::vec2(self.style.outer_margin, 0.0));

        let (mut base_geometries, mut boundaries) = self.geometries(root_rect, specs);
        let mut geometries = base_geometries.clone();
        let mut actions = Vec::new();
        let mut boundary_interaction = None;
        let mut extended_boundary_guide = None;
        let mut boundary_break_available = false;
        if self.split_placement.is_some() {
            if let Some(preview_action) =
                self.split_action_at_pointer(ui, &base_geometries, root_rect)
            {
                (geometries, boundaries) =
                    self.split_preview_geometries(root_rect, specs, preview_action);
            }
            self.paint_boundaries(ui, &boundaries);
        } else {
            (
                actions,
                boundary_interaction,
                extended_boundary_guide,
                boundary_break_available,
            ) = self.handle_boundaries(ui, &boundaries, root_rect);
            if actions
                .iter()
                .any(|action| matches!(action, LayoutAction::SetFraction { .. }))
            {
                for action in actions.drain(..) {
                    self.apply_action(action, specs);
                }
                (base_geometries, boundaries) = self.geometries(root_rect, specs);
                geometries.clone_from(&base_geometries);
            }
        }
        if self.split_placement.is_none()
            && let Some(action) = self.maximize_shortcut_action(ui, &base_geometries)
        {
            actions.push(action);
        }

        for geometry in &mut geometries {
            let (action, interaction_rect) =
                self.show_title_bar(ui, specs, geometry, &mut add_widget);
            geometry.title_interaction_rect = interaction_rect;
            if let Some(action) = action {
                actions.push(action);
            }
            let mut body_ui = ui.new_child(
                UiBuilder::new()
                    .id_salt(("panel-body", geometry.panel_id.as_str()))
                    .max_rect(geometry.body_rect)
                    .layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            body_ui.set_clip_rect(geometry.body_rect);
            add_widget(
                PanelSlot::Body {
                    panel_id: &geometry.panel_id,
                    content_id: &geometry.content_id,
                },
                &mut body_ui,
            );
            self.finish_panel(ui, geometry);
        }
        if let Some(action) = self.handle_split_placement(ui, &base_geometries, root_rect) {
            actions.push(action);
        }

        for action in actions {
            self.apply_action(action, specs);
        }
        if let Some((axis, coordinate)) = extended_boundary_guide {
            paint_extended_boundary_guide(ui, axis, root_rect, coordinate);
        }

        // Keep boundary geometry alive through the complete pass. egui's
        // context-menu state is tied to the stable split IDs, not this vector.
        drop(boundaries);
        PanelLayoutResponse {
            panels: geometries,
            footer_rect,
            boundary_interaction,
            boundary_break_available,
        }
    }

    fn synchronize(&mut self, specs: &[PanelSpec<'_>]) {
        if specs.is_empty() {
            self.state.root = None;
            self.state.maximized = None;
            return;
        }
        if self.state.root.is_none() {
            self.state.root = Some(LayoutNode::Panel {
                panel: PanelState {
                    id: specs[0].id.to_owned(),
                    content: specs[0].id.to_owned(),
                    title_bar_position: TitleBarPosition::Top,
                },
            });
        }

        let valid: HashSet<_> = specs.iter().map(|spec| spec.id).collect();
        let mut assigned_singletons = HashSet::new();
        visit_panels_mut(self.state.root.as_mut(), &mut |panel| {
            let current = specs.iter().find(|spec| spec.id == panel.content);
            let duplicate_singleton = current.is_some_and(|spec| {
                spec.singleton && !assigned_singletons.insert(spec.id.to_owned())
            });
            if !valid.contains(panel.content.as_str()) || duplicate_singleton {
                panel.content = available_content(specs, &assigned_singletons)
                    .unwrap_or(specs[0].id)
                    .to_owned();
                if specs
                    .iter()
                    .any(|spec| spec.id == panel.content && spec.singleton)
                {
                    assigned_singletons.insert(panel.content.clone());
                }
            }
        });

        if self
            .state
            .maximized
            .as_ref()
            .is_some_and(|id| find_panel(self.state.root.as_ref(), id).is_none())
        {
            self.state.maximized = None;
        }
    }

    fn geometries(
        &self,
        rect: Rect,
        specs: &[PanelSpec<'_>],
    ) -> (Vec<PanelGeometry>, Vec<BoundaryGeometry>) {
        let mut panels = Vec::new();
        let mut boundaries = Vec::new();
        let Some(root) = self.state.root.as_ref() else {
            return (panels, boundaries);
        };
        if let Some(maximized) = self.state.maximized.as_deref()
            && let Some(panel) = find_panel(Some(root), maximized)
        {
            push_panel_geometry(panel, rect, true, &self.style, &mut panels);
            return (panels, boundaries);
        }
        collect_geometries(root, rect, specs, &self.style, &mut panels, &mut boundaries);
        (panels, boundaries)
    }

    fn handle_boundaries(
        &mut self,
        ui: &mut Ui,
        boundaries: &[BoundaryGeometry],
        root_rect: Rect,
    ) -> BoundaryHandling {
        let mut actions = Vec::new();
        let mut interaction = None;
        let mut extended_boundary_guide = None;
        let mut break_available = false;
        for boundary in boundaries {
            let break_candidate =
                boundary_break_candidate(self.state.root.as_ref(), boundary.id, boundaries);
            let response = ui.interact(
                boundary.rect,
                ui.id().with(("panel-splitter", boundary.id)),
                Sense::click_and_drag(),
            );
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(match boundary.axis {
                    SplitAxis::Horizontal => CursorIcon::ResizeVertical,
                    SplitAxis::Vertical => CursorIcon::ResizeHorizontal,
                });
            }
            if response.dragged() {
                break_available = break_candidate.is_some();
                interaction = Some(
                    if boundary
                        .nearest_parallel_boundary(boundaries, BOUNDARY_EXTEND_DISTANCE)
                        .is_some()
                    {
                        BoundaryInteraction::DraggingWithParallelBoundary
                    } else {
                        BoundaryInteraction::Dragging
                    },
                );
            } else if response.hovered() && interaction.is_none() {
                interaction = Some(BoundaryInteraction::Hovered);
            }
            if response.dragged()
                && let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
            {
                let modifiers = ui.input(|input| input.modifiers);
                if modifiers.alt
                    && let Some(candidate) = break_candidate
                {
                    actions.push(LayoutAction::BreakSplit {
                        split_id: boundary.id,
                        band: if axis_coordinate(candidate.axis, pointer) < candidate.coordinate {
                            SplitSide::First
                        } else {
                            SplitSide::Second
                        },
                        crossing_fraction: candidate.crossing_fraction,
                    });
                }
                let resize_actions = boundary.resize_actions(
                    pointer,
                    boundaries,
                    root_rect,
                    modifiers.ctrl,
                    modifiers.shift && !modifiers.alt,
                );
                if modifiers.shift
                    && !modifiers.alt
                    && resize_actions.len() > 1
                    && let Some(LayoutAction::SetFraction { fraction, .. }) = resize_actions.first()
                {
                    extended_boundary_guide =
                        Some((boundary.axis, boundary.coordinate_for_fraction(*fraction)));
                }
                actions.extend(resize_actions);
            }
            if response.secondary_clicked() {
                self.boundary_context = Some(BoundaryContext {
                    split_id: boundary.id,
                    axis: boundary.axis,
                });
            }

            response.context_menu(|ui| {
                let Some(context) = self
                    .boundary_context
                    .as_ref()
                    .filter(|context| context.split_id == boundary.id)
                    .cloned()
                else {
                    ui.close();
                    return;
                };
                let ((first_label, first_keep), (second_label, second_keep)) =
                    join_options(context.axis);
                if ui.button(first_label).clicked() {
                    actions.push(LayoutAction::Join {
                        split_id: context.split_id,
                        keep: first_keep,
                    });
                    ui.close();
                }
                if ui.button(second_label).clicked() {
                    actions.push(LayoutAction::Join {
                        split_id: context.split_id,
                        keep: second_keep,
                    });
                    ui.close();
                }
                ui.separator();
                if ui.button("Horizontal Split").clicked() {
                    self.split_placement = Some(SplitPlacement::Panel {
                        axis: SplitAxis::Horizontal,
                    });
                    ui.close();
                }
                if ui.button("Vertical Split").clicked() {
                    self.split_placement = Some(SplitPlacement::Panel {
                        axis: SplitAxis::Vertical,
                    });
                    ui.close();
                }
            });

            ui.painter()
                .rect_filled(boundary.rect, 0.0, self.style.splitter_fill);
            if response.dragged() {
                let visual = boundary.visual_rect(self.style.splitter_visual_size);
                ui.painter()
                    .rect_filled(visual, 0.0, self.style.splitter_drag_fill);
            }
        }
        (
            actions,
            interaction,
            extended_boundary_guide,
            break_available,
        )
    }

    fn handle_split_placement(
        &mut self,
        ui: &mut Ui,
        panels: &[PanelGeometry],
        root_rect: Rect,
    ) -> Option<LayoutAction> {
        let placement = self.split_placement?;
        if ui.input(|input| {
            input.key_pressed(egui::Key::Escape) || input.pointer.secondary_clicked()
        }) {
            self.split_placement = None;
            return None;
        }

        match placement {
            SplitPlacement::Panel { axis } => {
                for panel in panels {
                    let response = ui.interact(
                        panel.panel_rect,
                        ui.id()
                            .with(("panel-split-placement", panel.panel_id.as_str())),
                        Sense::click(),
                    );
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(split_cursor(axis));
                    }
                    if response.clicked()
                        && let Some(pointer) = response.interact_pointer_pos()
                    {
                        self.split_placement = None;
                        return Some(LayoutAction::Split {
                            panel_id: panel.panel_id.clone(),
                            axis,
                            fraction: fraction_in_rect(axis, panel.panel_rect, pointer),
                        });
                    }
                }
            }
            SplitPlacement::Layout { side } => {
                let response = ui.interact(
                    root_rect,
                    ui.id().with("layout-split-placement"),
                    Sense::click(),
                );
                if response.hovered() {
                    ui.ctx().set_cursor_icon(split_cursor(side.axis()));
                }
                if response.clicked()
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    self.split_placement = None;
                    return Some(LayoutAction::SplitLayout {
                        side,
                        fraction: fraction_in_rect(side.axis(), root_rect, pointer),
                    });
                }
            }
        }
        None
    }

    fn split_action_at_pointer(
        &self,
        ui: &Ui,
        panels: &[PanelGeometry],
        root_rect: Rect,
    ) -> Option<LayoutAction> {
        let placement = self.split_placement.as_ref()?;
        let pointer = ui.input(|input| input.pointer.hover_pos())?;
        match *placement {
            SplitPlacement::Panel { axis } => {
                let panel = panel_at_pointer(panels, pointer)?;
                Some(LayoutAction::Split {
                    panel_id: panel.panel_id.clone(),
                    axis,
                    fraction: fraction_in_rect(axis, panel.panel_rect, pointer),
                })
            }
            SplitPlacement::Layout { side } if root_rect.contains(pointer) => {
                Some(LayoutAction::SplitLayout {
                    side,
                    fraction: fraction_in_rect(side.axis(), root_rect, pointer),
                })
            }
            SplitPlacement::Layout { .. } => None,
        }
    }

    fn split_preview_geometries(
        &self,
        rect: Rect,
        specs: &[PanelSpec<'_>],
        action: LayoutAction,
    ) -> (Vec<PanelGeometry>, Vec<BoundaryGeometry>) {
        let mut preview = self.clone();
        preview.split_placement = None;
        preview.apply_action(action, specs);
        preview.geometries(rect, specs)
    }

    fn paint_boundaries(&self, ui: &Ui, boundaries: &[BoundaryGeometry]) {
        for boundary in boundaries {
            ui.painter()
                .rect_filled(boundary.rect, 0.0, self.style.splitter_fill);
        }
    }

    fn maximize_shortcut_action(
        &self,
        ui: &mut Ui,
        panels: &[PanelGeometry],
    ) -> Option<LayoutAction> {
        let shortcut = self.maximize_shortcut?;
        let panel_id = self.state.maximized.clone().or_else(|| {
            let pointer = ui.input(|input| input.pointer.hover_pos())?;
            Some(panel_at_pointer(panels, pointer)?.panel_id.clone())
        })?;
        if !ui.input_mut(|input| input.consume_shortcut(&shortcut)) {
            return None;
        }
        Some(LayoutAction::Panel {
            panel_id,
            action: if self.state.maximized.is_some() {
                PanelAction::RestoreMaximized
            } else {
                PanelAction::Maximize
            },
        })
    }

    fn show_title_bar(
        &self,
        ui: &mut Ui,
        specs: &[PanelSpec<'_>],
        geometry: &PanelGeometry,
        add_widget: &mut impl FnMut(PanelSlot<'_>, &mut Ui),
    ) -> (Option<LayoutAction>, Option<Rect>) {
        let rounding = match geometry.title_bar_position {
            TitleBarPosition::Top => CornerRadius {
                nw: self.style.corner_radius,
                ne: self.style.corner_radius,
                sw: 0,
                se: 0,
            },
            TitleBarPosition::Bottom => CornerRadius {
                nw: 0,
                ne: 0,
                sw: self.style.corner_radius,
                se: self.style.corner_radius,
            },
        };
        ui.painter()
            .rect_filled(geometry.title_rect, rounding, self.style.title_fill);
        let divider = match geometry.title_bar_position {
            TitleBarPosition::Top => [
                geometry.title_rect.left_bottom(),
                geometry.title_rect.right_bottom(),
            ],
            TitleBarPosition::Bottom => [
                geometry.title_rect.left_top(),
                geometry.title_rect.right_top(),
            ],
        };
        ui.painter()
            .line_segment(divider, Stroke::new(1.0, self.style.border_color));

        let mut action = None;
        let mut title_ui = ui.new_child(
            UiBuilder::new()
                .id_salt(("panel-title-content", geometry.panel_id.as_str()))
                .max_rect(geometry.title_rect.shrink2(egui::vec2(6.0, 2.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let selected_spec = specs
            .iter()
            .find(|spec| spec.id == geometry.content_id)
            .copied();
        let selected_title = selected_spec.map_or(geometry.content_id.as_str(), |spec| spec.title);
        let selected_icon = selected_spec.map_or(PanelIcon::Panel, |spec| spec.icon);
        let selector =
            egui::ComboBox::from_id_salt(("panel-content-selector", geometry.panel_id.as_str()))
                .selected_text("   ")
                .width(44.0)
                .show_ui(&mut title_ui, |ui| {
                    ui.set_min_width(190.0);
                    for spec in specs {
                        let assigned_elsewhere = spec.singleton
                            && find_panel_by_content(self.state.root.as_ref(), spec.id)
                                .is_some_and(|panel| panel.id != geometry.panel_id);
                        let selected = spec.id == geometry.content_id;
                        if ui
                            .add_enabled(!assigned_elsewhere, panel_content_button(*spec, selected))
                            .clicked()
                        {
                            action = Some(LayoutAction::ChangeContent {
                                panel_id: geometry.panel_id.clone(),
                                content_id: spec.id.to_owned(),
                            });
                            ui.close();
                        }
                    }
                });
        let icon_color = title_ui
            .visuals()
            .widgets
            .style(&selector.response)
            .fg_stroke
            .color;
        let icon_rect = Rect::from_center_size(
            egui::pos2(
                selector.response.rect.left() + 12.0,
                selector.response.rect.center().y,
            ),
            egui::vec2(16.0, 16.0),
        );
        selected_icon.paint(&title_ui, icon_rect, icon_color);
        selector.response.on_hover_text(selected_title);
        add_widget(
            PanelSlot::TitleBar {
                panel_id: &geometry.panel_id,
                content_id: &geometry.content_id,
            },
            &mut title_ui,
        );
        let interaction_left = title_ui.available_rect_before_wrap().left();
        let maximize_response = title_ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                let response = panel_control_button(ui, maximize_icon, maximize_tooltip);
                if response.clicked() {
                    action = Some(LayoutAction::Panel {
                        panel_id: geometry.panel_id.clone(),
                        action: if geometry.maximized {
                            PanelAction::RestoreMaximized
                        } else {
                            PanelAction::Maximize
                        },
                    });
                }
                response
            })
            .inner;
        let interaction_rect = title_interaction_rect(
            geometry.title_rect,
            interaction_left,
            maximize_response.rect.left(),
        );
        if let Some(interaction_rect) = interaction_rect {
            let response = ui.interact(
                interaction_rect,
                ui.id().with(("panel-title", geometry.panel_id.as_str())),
                Sense::click(),
            );
            if response.hovered() {
                ui.painter()
                    .rect_filled(interaction_rect, 0.0, self.style.title_hover_fill);
            }
            if response.double_clicked() {
                action = Some(LayoutAction::Panel {
                    panel_id: geometry.panel_id.clone(),
                    action: if geometry.maximized {
                        PanelAction::RestoreMaximized
                    } else {
                        PanelAction::Maximize
                    },
                });
            }
            response.context_menu(|ui| {
                self.show_area_menu(ui, geometry, &mut action);
            });
        }
        ui.painter()
            .line_segment(divider, Stroke::new(1.0, self.style.border_color));
        (action, interaction_rect)
    }

    fn show_area_menu(
        &self,
        ui: &mut Ui,
        geometry: &PanelGeometry,
        action: &mut Option<LayoutAction>,
    ) {
        ui.set_min_width(180.0);
        let flip_label = match geometry.title_bar_position {
            TitleBarPosition::Top => "Flip to Bottom",
            TitleBarPosition::Bottom => "Flip to Top",
        };
        let flip_icon = match geometry.title_bar_position {
            TitleBarPosition::Top => "↓",
            TitleBarPosition::Bottom => "↑",
        };
        if area_menu_button(ui, flip_label, flip_icon, None, true).clicked() {
            *action = Some(LayoutAction::Panel {
                panel_id: geometry.panel_id.clone(),
                action: PanelAction::FlipTitleBar,
            });
            ui.close();
        }
        ui.separator();
        let split_label = menu_item_layout_job(ui, Some("▣"), "Split This Area");
        ui.menu_button(egui::WidgetText::LayoutJob(split_label.into()), |ui| {
            ui.set_min_width(180.0);
            if area_menu_button(ui, "Horizontal Split", "═", None, true).clicked() {
                *action = Some(LayoutAction::BeginSplit {
                    axis: SplitAxis::Horizontal,
                });
                ui.close();
            }
            if area_menu_button(ui, "Vertical Split", "║", None, true).clicked() {
                *action = Some(LayoutAction::BeginSplit {
                    axis: SplitAxis::Vertical,
                });
                ui.close();
            }
        });
        let add_label = menu_item_layout_job(ui, Some("+"), "Add Area to Layout");
        ui.menu_button(egui::WidgetText::LayoutJob(add_label.into()), |ui| {
            ui.set_min_width(180.0);
            for (label, icon, side) in [
                ("Left", "←", LayoutSide::Left),
                ("Right", "→", LayoutSide::Right),
                ("Top", "↑", LayoutSide::Top),
                ("Bottom", "↓", LayoutSide::Bottom),
            ] {
                if area_menu_button(ui, label, icon, None, true).clicked() {
                    *action = Some(LayoutAction::BeginLayoutSplit { side });
                    ui.close();
                }
            }
        });
        let maximize_label = if geometry.maximized {
            "Restore Area"
        } else {
            "Maximize Area"
        };
        let maximize_icon = if geometry.maximized { "▣" } else { "□" };
        if area_menu_button(
            ui,
            maximize_label,
            maximize_icon,
            self.maximize_shortcut.map(MenuShortcut::from_keyboard),
            true,
        )
        .clicked()
        {
            *action = Some(LayoutAction::Panel {
                panel_id: geometry.panel_id.clone(),
                action: if geometry.maximized {
                    PanelAction::RestoreMaximized
                } else {
                    PanelAction::Maximize
                },
            });
            ui.close();
        }
        ui.separator();
        if area_menu_button(
            ui,
            "Close Area",
            "×",
            None,
            all_panels(self.state.root.as_ref()).len() > 1,
        )
        .on_disabled_hover_text("The last area cannot be closed")
        .clicked()
        {
            *action = Some(LayoutAction::Panel {
                panel_id: geometry.panel_id.clone(),
                action: PanelAction::Close,
            });
            ui.close();
        }
    }

    fn finish_panel(&self, ui: &Ui, geometry: &PanelGeometry) {
        let rounding = CornerRadius::same(self.style.corner_radius);
        if geometry.panel_rect.height() > f32::from(self.style.corner_radius) {
            let radius = f32::from(self.style.corner_radius);
            let (cap, cap_rounding) = match geometry.title_bar_position {
                TitleBarPosition::Top => (
                    Rect::from_min_max(
                        egui::pos2(
                            geometry.panel_rect.left(),
                            geometry.panel_rect.bottom() - radius,
                        ),
                        geometry.panel_rect.right_bottom(),
                    ),
                    CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: self.style.corner_radius,
                        se: self.style.corner_radius,
                    },
                ),
                TitleBarPosition::Bottom => (
                    Rect::from_min_max(
                        geometry.panel_rect.left_top(),
                        egui::pos2(
                            geometry.panel_rect.right(),
                            geometry.panel_rect.top() + radius,
                        ),
                    ),
                    CornerRadius {
                        nw: self.style.corner_radius,
                        ne: self.style.corner_radius,
                        sw: 0,
                        se: 0,
                    },
                ),
            };
            ui.painter()
                .rect_filled(cap, cap_rounding, self.style.panel_fill);
        }
        ui.painter().rect_stroke(
            geometry.panel_rect,
            rounding,
            Stroke::new(1.0, self.style.border_color),
            StrokeKind::Inside,
        );
    }

    fn apply_action(&mut self, action: LayoutAction, specs: &[PanelSpec<'_>]) {
        match action {
            LayoutAction::SetFraction { split_id, fraction } => {
                set_split_fraction(self.state.root.as_mut(), split_id, fraction);
            }
            LayoutAction::Join { split_id, keep } => {
                if self.state.maximized.is_some() {
                    self.restore_maximized();
                }
                join_split(self.state.root.as_mut(), split_id, keep);
            }
            LayoutAction::BreakSplit {
                split_id,
                band,
                crossing_fraction,
            } => {
                break_split(self.state.root.as_mut(), split_id, band, crossing_fraction);
            }
            LayoutAction::Split {
                panel_id,
                axis,
                fraction,
            } => {
                if self.state.maximized.is_some() {
                    self.restore_maximized();
                }
                let assigned = assigned_singletons(self.state.root.as_ref(), specs);
                let Some(content) = available_content(specs, &assigned).map(str::to_owned) else {
                    return;
                };
                let new_panel_id = self.allocate_id("panel");
                let split_id = self.allocate_numeric_id();
                split_panel(
                    self.state.root.as_mut(),
                    &panel_id,
                    axis,
                    fraction,
                    split_id,
                    PanelState {
                        id: new_panel_id,
                        content,
                        title_bar_position: TitleBarPosition::Top,
                    },
                );
            }
            LayoutAction::SplitLayout { side, fraction } => {
                if self.state.maximized.is_some() {
                    self.restore_maximized();
                }
                let assigned = assigned_singletons(self.state.root.as_ref(), specs);
                let Some(content) = available_content(specs, &assigned).map(str::to_owned) else {
                    return;
                };
                let new_panel_id = self.allocate_id("panel");
                let split_id = self.allocate_numeric_id();
                let Some(existing) = self.state.root.take() else {
                    return;
                };
                let new_panel = LayoutNode::Panel {
                    panel: PanelState {
                        id: new_panel_id,
                        content,
                        title_bar_position: TitleBarPosition::Top,
                    },
                };
                let (first, second) = if side.new_area_is_first() {
                    (new_panel, existing)
                } else {
                    (existing, new_panel)
                };
                self.state.root = Some(LayoutNode::Split {
                    id: split_id,
                    axis: side.axis(),
                    fraction: fraction.clamp(0.1, 0.9),
                    first: Box::new(first),
                    second: Box::new(second),
                });
            }
            LayoutAction::BeginSplit { axis } => {
                self.split_placement = Some(SplitPlacement::Panel { axis });
            }
            LayoutAction::BeginLayoutSplit { side } => {
                if self.state.maximized.is_some() {
                    self.restore_maximized();
                }
                self.split_placement = Some(SplitPlacement::Layout { side });
            }
            LayoutAction::ChangeContent {
                panel_id,
                content_id,
            } => {
                if let Some(panel) = find_panel_mut(self.state.root.as_mut(), &panel_id) {
                    panel.content = content_id;
                }
            }
            LayoutAction::Panel { panel_id, action } => {
                self.apply_panel_action(&panel_id, action);
            }
        }
    }

    fn apply_panel_action(&mut self, panel_id: &str, action: PanelAction) {
        match action {
            PanelAction::FlipTitleBar => {
                if let Some(panel) = find_panel_mut(self.state.root.as_mut(), panel_id) {
                    panel.title_bar_position = match panel.title_bar_position {
                        TitleBarPosition::Top => TitleBarPosition::Bottom,
                        TitleBarPosition::Bottom => TitleBarPosition::Top,
                    };
                }
            }
            PanelAction::Maximize => {
                self.state.maximized = Some(panel_id.to_owned());
            }
            PanelAction::RestoreMaximized => self.restore_maximized(),
            PanelAction::Close => {
                if all_panels(self.state.root.as_ref()).len() <= 1 {
                    return;
                }
                self.restore_maximized();
                remove_panel(self.state.root.as_mut(), panel_id);
            }
        }
    }

    fn restore_maximized(&mut self) {
        self.state.maximized = None;
    }

    fn allocate_numeric_id(&mut self) -> u64 {
        let id = self.state.next_id;
        self.state.next_id += 1;
        id
    }

    fn allocate_id(&mut self, prefix: &str) -> String {
        loop {
            let id = format!("{prefix}-{}", self.allocate_numeric_id());
            if find_panel(self.state.root.as_ref(), &id).is_none() {
                return id;
            }
        }
    }
}

fn area_menu_button(
    ui: &mut Ui,
    label: &str,
    icon: &str,
    shortcut: Option<MenuShortcut>,
    enabled: bool,
) -> egui::Response {
    let job = menu_item_layout_job(ui, Some(icon), label);
    let mut button = egui::Button::new(egui::WidgetText::LayoutJob(job.into()))
        .wrap_mode(egui::TextWrapMode::Extend);
    if let Some(shortcut) = shortcut {
        button = button.right_text(shortcut.to_string());
    }
    ui.add_enabled(enabled, button)
}

/// Compatibility alias for hosts that used the original flat vertical
/// manager. The implementation now supports arbitrary nested splits.
pub type VerticalPanelLayout = PanelLayout;

#[derive(Debug, Clone)]
struct BoundaryContext {
    split_id: u64,
    axis: SplitAxis,
}

#[derive(Debug, Clone, Copy)]
enum SplitPlacement {
    Panel { axis: SplitAxis },
    Layout { side: LayoutSide },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutSide {
    Left,
    Right,
    Top,
    Bottom,
}

impl LayoutSide {
    const fn axis(self) -> SplitAxis {
        match self {
            Self::Left | Self::Right => SplitAxis::Vertical,
            Self::Top | Self::Bottom => SplitAxis::Horizontal,
        }
    }

    const fn new_area_is_first(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }
}

#[derive(Debug, Clone)]
struct BoundaryGeometry {
    id: u64,
    axis: SplitAxis,
    rect: Rect,
    parent_rect: Rect,
}

impl BoundaryGeometry {
    fn fraction_at(&self, pointer: egui::Pos2) -> f32 {
        self.fraction_for_coordinate(axis_coordinate(self.axis, pointer))
    }

    fn snapped_fraction_at(
        &self,
        pointer: egui::Pos2,
        boundaries: &[BoundaryGeometry],
        root_rect: Rect,
    ) -> f32 {
        self.snapped_fraction_at_excluding(pointer, boundaries, root_rect, None)
    }

    fn snapped_fraction_at_excluding(
        &self,
        pointer: egui::Pos2,
        boundaries: &[BoundaryGeometry],
        root_rect: Rect,
        excluded_boundary: Option<u64>,
    ) -> f32 {
        self.fraction_for_coordinate(self.snapped_coordinate_at(
            pointer,
            boundaries,
            root_rect,
            excluded_boundary,
        ))
    }

    fn snapped_coordinate_at(
        &self,
        pointer: egui::Pos2,
        boundaries: &[BoundaryGeometry],
        root_rect: Rect,
        excluded_boundary: Option<u64>,
    ) -> f32 {
        let coordinate = axis_coordinate(self.axis, pointer);
        self.nearest_parallel_boundary_to_excluding(
            boundaries,
            BOUNDARY_ALIGNMENT_DISTANCE,
            coordinate,
            excluded_boundary,
        )
        .map(BoundaryGeometry::coordinate)
        .unwrap_or_else(|| {
            let origin = axis_min(self.axis, root_rect);
            origin + ((coordinate - origin) / BOUNDARY_SNAP_GRID).round() * BOUNDARY_SNAP_GRID
        })
    }

    fn nearest_parallel_boundary<'a>(
        &self,
        boundaries: &'a [BoundaryGeometry],
        maximum_distance: f32,
    ) -> Option<&'a BoundaryGeometry> {
        self.nearest_parallel_boundary_to_excluding(
            boundaries,
            maximum_distance,
            self.coordinate(),
            None,
        )
    }

    fn nearest_parallel_boundary_to_excluding<'a>(
        &self,
        boundaries: &'a [BoundaryGeometry],
        maximum_distance: f32,
        coordinate: f32,
        excluded_boundary: Option<u64>,
    ) -> Option<&'a BoundaryGeometry> {
        boundaries
            .iter()
            .filter(|candidate| {
                candidate.id != self.id
                    && Some(candidate.id) != excluded_boundary
                    && candidate.axis == self.axis
            })
            .min_by(|left, right| {
                (coordinate - left.coordinate())
                    .abs()
                    .total_cmp(&(coordinate - right.coordinate()).abs())
            })
            .filter(|candidate| (coordinate - candidate.coordinate()).abs() <= maximum_distance)
    }

    fn resize_actions(
        &self,
        pointer: egui::Pos2,
        boundaries: &[BoundaryGeometry],
        root_rect: Rect,
        snap: bool,
        extend: bool,
    ) -> Vec<LayoutAction> {
        let parallel = extend
            .then(|| self.nearest_parallel_boundary(boundaries, BOUNDARY_EXTEND_DISTANCE))
            .flatten();
        let target_fraction = if snap {
            if let Some(parallel) = parallel {
                self.snapped_fraction_at_excluding(
                    pointer,
                    boundaries,
                    root_rect,
                    Some(parallel.id),
                )
            } else {
                self.snapped_fraction_at(pointer, boundaries, root_rect)
            }
        } else {
            self.fraction_at(pointer)
        };
        let mut actions = vec![LayoutAction::SetFraction {
            split_id: self.id,
            fraction: target_fraction,
        }];
        if let Some(parallel) = parallel {
            let coordinate = self.coordinate_for_fraction(target_fraction);
            actions.push(LayoutAction::SetFraction {
                split_id: parallel.id,
                fraction: parallel.fraction_for_coordinate(coordinate),
            });
        }
        actions
    }

    fn coordinate(&self) -> f32 {
        axis_coordinate(self.axis, self.rect.center())
    }

    fn fraction_for_coordinate(&self, coordinate: f32) -> f32 {
        let parent_extent = axis_extent(self.axis, self.parent_rect);
        let splitter_extent = axis_extent(self.axis, self.rect);
        let usable = (parent_extent - splitter_extent).max(1.0);
        ((coordinate - axis_min(self.axis, self.parent_rect) - splitter_extent * 0.5) / usable)
            .clamp(0.1, 0.9)
    }

    fn coordinate_for_fraction(&self, fraction: f32) -> f32 {
        let parent_extent = axis_extent(self.axis, self.parent_rect);
        let splitter_extent = axis_extent(self.axis, self.rect);
        let usable = (parent_extent - splitter_extent).max(1.0);
        axis_min(self.axis, self.parent_rect)
            + splitter_extent * 0.5
            + usable * fraction.clamp(0.1, 0.9)
    }

    fn visual_rect(&self, thickness: f32) -> Rect {
        match self.axis {
            SplitAxis::Horizontal => {
                Rect::from_center_size(self.rect.center(), egui::vec2(self.rect.width(), thickness))
            }
            SplitAxis::Vertical => Rect::from_center_size(
                self.rect.center(),
                egui::vec2(thickness, self.rect.height()),
            ),
        }
    }
}

#[derive(Clone, Copy)]
struct BoundaryBreakCandidate {
    axis: SplitAxis,
    coordinate: f32,
    crossing_fraction: f32,
}

fn boundary_break_candidate(
    root: Option<&LayoutNode>,
    split_id: u64,
    boundaries: &[BoundaryGeometry],
) -> Option<BoundaryBreakCandidate> {
    let (axis, first_id, second_id) = breakable_child_splits(root?, split_id)?;
    let first = boundaries.iter().find(|boundary| boundary.id == first_id)?;
    let second = boundaries
        .iter()
        .find(|boundary| boundary.id == second_id)?;
    let coordinate = (first.coordinate() + second.coordinate()) * 0.5;
    if (first.coordinate() - second.coordinate()).abs() > BOUNDARY_EXTEND_DISTANCE {
        return None;
    }
    Some(BoundaryBreakCandidate {
        axis,
        coordinate,
        crossing_fraction: first.fraction_for_coordinate(coordinate),
    })
}

fn breakable_child_splits(node: &LayoutNode, split_id: u64) -> Option<(SplitAxis, u64, u64)> {
    match node {
        LayoutNode::Split {
            id,
            axis,
            first,
            second,
            ..
        } if *id == split_id => {
            let LayoutNode::Split {
                id: first_id,
                axis: first_axis,
                ..
            } = first.as_ref()
            else {
                return None;
            };
            let LayoutNode::Split {
                id: second_id,
                axis: second_axis,
                ..
            } = second.as_ref()
            else {
                return None;
            };
            (*first_axis == *second_axis && *first_axis != *axis).then_some((
                *first_axis,
                *first_id,
                *second_id,
            ))
        }
        LayoutNode::Split { first, second, .. } => breakable_child_splits(first, split_id)
            .or_else(|| breakable_child_splits(second, split_id)),
        LayoutNode::Panel { .. } => None,
    }
}

fn paint_extended_boundary_guide(ui: &Ui, axis: SplitAxis, root_rect: Rect, coordinate: f32) {
    let points = match axis {
        SplitAxis::Horizontal => [
            egui::pos2(root_rect.left(), coordinate),
            egui::pos2(root_rect.right(), coordinate),
        ],
        SplitAxis::Vertical => [
            egui::pos2(coordinate, root_rect.top()),
            egui::pos2(coordinate, root_rect.bottom()),
        ],
    };
    ui.painter()
        .line_segment(points, Stroke::new(2.0, Color32::from_rgb(185, 185, 185)));
}

fn axis_coordinate(axis: SplitAxis, position: egui::Pos2) -> f32 {
    match axis {
        SplitAxis::Horizontal => position.y,
        SplitAxis::Vertical => position.x,
    }
}

fn axis_min(axis: SplitAxis, rect: Rect) -> f32 {
    match axis {
        SplitAxis::Horizontal => rect.top(),
        SplitAxis::Vertical => rect.left(),
    }
}

fn axis_extent(axis: SplitAxis, rect: Rect) -> f32 {
    match axis {
        SplitAxis::Horizontal => rect.height(),
        SplitAxis::Vertical => rect.width(),
    }
}

#[derive(Debug, Clone)]
enum LayoutAction {
    SetFraction {
        split_id: u64,
        fraction: f32,
    },
    Join {
        split_id: u64,
        keep: SplitSide,
    },
    BreakSplit {
        split_id: u64,
        band: SplitSide,
        crossing_fraction: f32,
    },
    Split {
        panel_id: String,
        axis: SplitAxis,
        fraction: f32,
    },
    SplitLayout {
        side: LayoutSide,
        fraction: f32,
    },
    BeginSplit {
        axis: SplitAxis,
    },
    BeginLayoutSplit {
        side: LayoutSide,
    },
    ChangeContent {
        panel_id: String,
        content_id: String,
    },
    Panel {
        panel_id: String,
        action: PanelAction,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitSide {
    First,
    Second,
}

fn split_cursor(axis: SplitAxis) -> CursorIcon {
    match axis {
        SplitAxis::Horizontal => CursorIcon::ResizeVertical,
        SplitAxis::Vertical => CursorIcon::ResizeHorizontal,
    }
}

fn join_options(axis: SplitAxis) -> ((&'static str, SplitSide), (&'static str, SplitSide)) {
    // The label names the direction the surviving panel expands. For
    // example, Join Right keeps the left/first panel and grows it rightward.
    match axis {
        SplitAxis::Horizontal => (
            ("Join Up", SplitSide::Second),
            ("Join Down", SplitSide::First),
        ),
        SplitAxis::Vertical => (
            ("Join Left", SplitSide::Second),
            ("Join Right", SplitSide::First),
        ),
    }
}

#[derive(Debug, Clone, Copy)]
enum PanelAction {
    FlipTitleBar,
    Maximize,
    RestoreMaximized,
    Close,
}

fn build_vertical_tree(panels: &[(String, f32)], next_id: &mut u64) -> Option<LayoutNode> {
    let (first, rest) = panels.split_first()?;
    let first_node = LayoutNode::Panel {
        panel: PanelState {
            id: first.0.clone(),
            content: first.0.clone(),
            title_bar_position: TitleBarPosition::Top,
        },
    };
    if rest.is_empty() {
        return Some(first_node);
    }
    let rest_weight: f32 = rest.iter().map(|(_, weight)| *weight).sum();
    let id = *next_id;
    *next_id += 1;
    Some(LayoutNode::Split {
        id,
        axis: SplitAxis::Horizontal,
        fraction: first.1 / (first.1 + rest_weight).max(0.001),
        first: Box::new(first_node),
        second: Box::new(build_vertical_tree(rest, next_id)?),
    })
}

fn build_panel_column(panels: &[PanelState], split_ids: &[u64]) -> LayoutNode {
    let (first, rest) = panels
        .split_first()
        .expect("a panel column must contain at least one panel");
    let first = LayoutNode::Panel {
        panel: first.clone(),
    };
    if rest.is_empty() {
        return first;
    }
    LayoutNode::Split {
        id: split_ids[0],
        axis: SplitAxis::Horizontal,
        fraction: 1.0 / panels.len() as f32,
        first: Box::new(first),
        second: Box::new(build_panel_column(rest, &split_ids[1..])),
    }
}

fn max_layout_id(node: Option<&LayoutNode>) -> u64 {
    match node {
        Some(LayoutNode::Panel { panel }) => panel
            .id
            .strip_prefix("panel-")
            .and_then(|number| number.parse().ok())
            .unwrap_or_default(),
        Some(LayoutNode::Split {
            id, first, second, ..
        }) => (*id)
            .max(max_layout_id(Some(first)))
            .max(max_layout_id(Some(second))),
        None => 0,
    }
}

fn collect_geometries(
    node: &LayoutNode,
    rect: Rect,
    specs: &[PanelSpec<'_>],
    style: &PanelLayoutStyle,
    panels: &mut Vec<PanelGeometry>,
    boundaries: &mut Vec<BoundaryGeometry>,
) {
    match node {
        LayoutNode::Panel { panel } => {
            push_panel_geometry(panel, rect, false, style, panels);
        }
        LayoutNode::Split {
            id,
            axis,
            fraction,
            first,
            second,
        } => {
            let (first_rect, splitter_rect, second_rect) = split_rects(
                rect,
                *axis,
                *fraction,
                minimum_size(first, specs, style),
                minimum_size(second, specs, style),
                style.splitter_size,
            );
            boundaries.push(BoundaryGeometry {
                id: *id,
                axis: *axis,
                rect: splitter_rect,
                parent_rect: rect,
            });
            collect_geometries(first, first_rect, specs, style, panels, boundaries);
            collect_geometries(second, second_rect, specs, style, panels, boundaries);
        }
    }
}

fn push_panel_geometry(
    panel: &PanelState,
    allocated_rect: Rect,
    maximized: bool,
    style: &PanelLayoutStyle,
    panels: &mut Vec<PanelGeometry>,
) {
    let title_height = style.title_height.min(allocated_rect.height());
    let title_rect = match panel.title_bar_position {
        TitleBarPosition::Top => Rect::from_min_size(
            allocated_rect.min,
            egui::vec2(allocated_rect.width(), title_height),
        ),
        TitleBarPosition::Bottom => Rect::from_min_size(
            egui::pos2(
                allocated_rect.left(),
                allocated_rect.bottom() - title_height,
            ),
            egui::vec2(allocated_rect.width(), title_height),
        ),
    };
    let radius = f32::from(style.corner_radius);
    let body_height = (allocated_rect.height() - title_height - radius).max(0.0);
    let body_min = match panel.title_bar_position {
        TitleBarPosition::Top => title_rect.left_bottom(),
        TitleBarPosition::Bottom => {
            egui::pos2(allocated_rect.left(), allocated_rect.top() + radius)
        }
    };
    panels.push(PanelGeometry {
        panel_id: panel.id.clone(),
        content_id: panel.content.clone(),
        title_rect,
        title_interaction_rect: None,
        body_rect: Rect::from_min_size(body_min, egui::vec2(allocated_rect.width(), body_height)),
        panel_rect: allocated_rect,
        allocated_rect,
        title_bar_position: panel.title_bar_position,
        maximized,
    });
}

fn title_interaction_rect(
    title_rect: Rect,
    content_right: f32,
    controls_left: f32,
) -> Option<Rect> {
    let left = content_right.clamp(title_rect.left(), title_rect.right());
    let right = controls_left.clamp(title_rect.left(), title_rect.right());
    (right > left).then(|| {
        Rect::from_min_max(
            egui::pos2(left, title_rect.top()),
            egui::pos2(right, title_rect.bottom()),
        )
    })
}

fn panel_at_pointer(panels: &[PanelGeometry], pointer: egui::Pos2) -> Option<&PanelGeometry> {
    panels
        .iter()
        .find(|panel| panel.panel_rect.contains(pointer))
}

fn split_rects(
    rect: Rect,
    axis: SplitAxis,
    fraction: f32,
    first_minimum: egui::Vec2,
    second_minimum: egui::Vec2,
    splitter_size: f32,
) -> (Rect, Rect, Rect) {
    let total = match axis {
        SplitAxis::Horizontal => rect.height(),
        SplitAxis::Vertical => rect.width(),
    };
    let usable = (total - splitter_size).max(0.0);
    let first_minimum = match axis {
        SplitAxis::Horizontal => first_minimum.y,
        SplitAxis::Vertical => first_minimum.x,
    }
    .min(usable);
    let second_minimum = match axis {
        SplitAxis::Horizontal => second_minimum.y,
        SplitAxis::Vertical => second_minimum.x,
    }
    .min(usable);
    let mut first_extent = usable * fraction.clamp(0.0, 1.0);
    if first_minimum + second_minimum <= usable {
        first_extent = first_extent.clamp(first_minimum, usable - second_minimum);
    }
    let second_extent = (usable - first_extent).max(0.0);
    match axis {
        SplitAxis::Horizontal => {
            let first = Rect::from_min_size(rect.min, egui::vec2(rect.width(), first_extent));
            let splitter =
                Rect::from_min_size(first.left_bottom(), egui::vec2(rect.width(), splitter_size));
            let second = Rect::from_min_size(
                splitter.left_bottom(),
                egui::vec2(rect.width(), second_extent),
            );
            (first, splitter, second)
        }
        SplitAxis::Vertical => {
            let first = Rect::from_min_size(rect.min, egui::vec2(first_extent, rect.height()));
            let splitter =
                Rect::from_min_size(first.right_top(), egui::vec2(splitter_size, rect.height()));
            let second = Rect::from_min_size(
                splitter.right_top(),
                egui::vec2(second_extent, rect.height()),
            );
            (first, splitter, second)
        }
    }
}

fn minimum_size(
    node: &LayoutNode,
    specs: &[PanelSpec<'_>],
    style: &PanelLayoutStyle,
) -> egui::Vec2 {
    match node {
        LayoutNode::Panel { panel } => {
            let spec = specs.iter().find(|spec| spec.id == panel.content);
            let width = spec.map_or(100.0, |spec| spec.minimum_width);
            let height = spec
                .map_or(100.0, |spec| spec.minimum_height)
                .max(style.title_height);
            egui::vec2(width, height)
        }
        LayoutNode::Split {
            axis,
            first,
            second,
            ..
        } => {
            let first = minimum_size(first, specs, style);
            let second = minimum_size(second, specs, style);
            match axis {
                SplitAxis::Horizontal => egui::vec2(
                    first.x.max(second.x),
                    first.y + style.splitter_size + second.y,
                ),
                SplitAxis::Vertical => egui::vec2(
                    first.x + style.splitter_size + second.x,
                    first.y.max(second.y),
                ),
            }
        }
    }
}

fn fraction_in_rect(axis: SplitAxis, rect: Rect, pointer: egui::Pos2) -> f32 {
    let fraction = match axis {
        SplitAxis::Horizontal => (pointer.y - rect.top()) / rect.height().max(1.0),
        SplitAxis::Vertical => (pointer.x - rect.left()) / rect.width().max(1.0),
    };
    fraction.clamp(0.1, 0.9)
}

fn find_content_split_fraction(
    node: &LayoutNode,
    first_content: &str,
    second_content: &str,
) -> Option<f32> {
    let LayoutNode::Split {
        fraction,
        first,
        second,
        ..
    } = node
    else {
        return None;
    };
    if contains_content(first, first_content) && contains_content(second, second_content) {
        return Some(*fraction);
    }
    if contains_content(first, second_content) && contains_content(second, first_content) {
        return Some(1.0 - *fraction);
    }
    find_content_split_fraction(first, first_content, second_content)
        .or_else(|| find_content_split_fraction(second, first_content, second_content))
}

fn contains_content(node: &LayoutNode, content: &str) -> bool {
    match node {
        LayoutNode::Panel { panel } => panel.content == content,
        LayoutNode::Split { first, second, .. } => {
            contains_content(first, content) || contains_content(second, content)
        }
    }
}

fn find_panel<'a>(node: Option<&'a LayoutNode>, id: &str) -> Option<&'a PanelState> {
    match node? {
        LayoutNode::Panel { panel } => (panel.id == id).then_some(panel),
        LayoutNode::Split { first, second, .. } => {
            find_panel(Some(first), id).or_else(|| find_panel(Some(second), id))
        }
    }
}

fn find_panel_mut<'a>(node: Option<&'a mut LayoutNode>, id: &str) -> Option<&'a mut PanelState> {
    match node? {
        LayoutNode::Panel { panel } => (panel.id == id).then_some(panel),
        LayoutNode::Split { first, second, .. } => {
            find_panel_mut(Some(first), id).or_else(|| find_panel_mut(Some(second), id))
        }
    }
}

fn find_panel_by_content<'a>(
    node: Option<&'a LayoutNode>,
    content: &str,
) -> Option<&'a PanelState> {
    match node? {
        LayoutNode::Panel { panel } => (panel.content == content).then_some(panel),
        LayoutNode::Split { first, second, .. } => find_panel_by_content(Some(first), content)
            .or_else(|| find_panel_by_content(Some(second), content)),
    }
}

fn visit_panels_mut(node: Option<&mut LayoutNode>, visitor: &mut impl FnMut(&mut PanelState)) {
    match node {
        Some(LayoutNode::Panel { panel }) => visitor(panel),
        Some(LayoutNode::Split { first, second, .. }) => {
            visit_panels_mut(Some(first), visitor);
            visit_panels_mut(Some(second), visitor);
        }
        None => {}
    }
}

fn all_panels(node: Option<&LayoutNode>) -> Vec<&PanelState> {
    let mut result = Vec::new();
    fn collect<'a>(node: Option<&'a LayoutNode>, result: &mut Vec<&'a PanelState>) {
        match node {
            Some(LayoutNode::Panel { panel }) => result.push(panel),
            Some(LayoutNode::Split { first, second, .. }) => {
                collect(Some(first), result);
                collect(Some(second), result);
            }
            None => {}
        }
    }
    collect(node, &mut result);
    result
}

fn subtree_contains_only(node: &LayoutNode, contents: &[&str]) -> bool {
    all_panels(Some(node))
        .into_iter()
        .all(|panel| contents.contains(&panel.content.as_str()))
}

fn assigned_singletons(node: Option<&LayoutNode>, specs: &[PanelSpec<'_>]) -> HashSet<String> {
    all_panels(node)
        .into_iter()
        .filter(|panel| {
            specs
                .iter()
                .any(|spec| spec.id == panel.content && spec.singleton)
        })
        .map(|panel| panel.content.clone())
        .collect()
}

fn available_content<'a>(
    specs: &'a [PanelSpec<'a>],
    assigned_singletons: &HashSet<String>,
) -> Option<&'a str> {
    specs
        .iter()
        .find(|spec| !spec.singleton || !assigned_singletons.contains(spec.id))
        .map(|spec| spec.id)
}

fn set_split_fraction(node: Option<&mut LayoutNode>, split_id: u64, fraction: f32) -> bool {
    match node {
        Some(LayoutNode::Split {
            id,
            fraction: current,
            first,
            second,
            ..
        }) => {
            if *id == split_id {
                *current = fraction.clamp(0.0, 1.0);
                true
            } else {
                set_split_fraction(Some(first), split_id, fraction)
                    || set_split_fraction(Some(second), split_id, fraction)
            }
        }
        _ => false,
    }
}

fn break_split(
    node: Option<&mut LayoutNode>,
    split_id: u64,
    dragged_band: SplitSide,
    crossing_fraction: f32,
) -> bool {
    let Some(node) = node else {
        return false;
    };
    if !matches!(node, LayoutNode::Split { id, .. } if *id == split_id) {
        return match node {
            LayoutNode::Split { first, second, .. } => {
                break_split(Some(first), split_id, dragged_band, crossing_fraction)
                    || break_split(Some(second), split_id, dragged_band, crossing_fraction)
            }
            LayoutNode::Panel { .. } => false,
        };
    }
    let snapshot = node.clone();
    match snapshot {
        LayoutNode::Split {
            id,
            axis: outer_axis,
            fraction: outer_fraction,
            first,
            second,
        } if id == split_id => {
            let LayoutNode::Split {
                id: first_child_id,
                axis: inner_axis,
                first: first_first,
                second: first_second,
                ..
            } = *first
            else {
                return false;
            };
            let LayoutNode::Split {
                id: second_child_id,
                axis: second_axis,
                first: second_first,
                second: second_second,
                ..
            } = *second
            else {
                return false;
            };
            if inner_axis == outer_axis || second_axis != inner_axis {
                return false;
            }
            let (first_band_id, second_band_id) = match dragged_band {
                SplitSide::First => (id, second_child_id),
                SplitSide::Second => (second_child_id, id),
            };
            *node = LayoutNode::Split {
                id: first_child_id,
                axis: inner_axis,
                fraction: crossing_fraction.clamp(0.1, 0.9),
                first: Box::new(LayoutNode::Split {
                    id: first_band_id,
                    axis: outer_axis,
                    fraction: outer_fraction,
                    first: first_first,
                    second: second_first,
                }),
                second: Box::new(LayoutNode::Split {
                    id: second_band_id,
                    axis: outer_axis,
                    fraction: outer_fraction,
                    first: first_second,
                    second: second_second,
                }),
            };
            true
        }
        LayoutNode::Split { .. } | LayoutNode::Panel { .. } => false,
    }
}

fn join_split(node: Option<&mut LayoutNode>, split_id: u64, keep: SplitSide) -> bool {
    let Some(node) = node else {
        return false;
    };
    match node {
        LayoutNode::Split {
            id, first, second, ..
        } if *id == split_id => {
            *node = match keep {
                SplitSide::First => (**first).clone(),
                SplitSide::Second => (**second).clone(),
            };
            true
        }
        LayoutNode::Split { first, second, .. } => {
            join_split(Some(first), split_id, keep) || join_split(Some(second), split_id, keep)
        }
        LayoutNode::Panel { .. } => false,
    }
}

fn remove_panel(node: Option<&mut LayoutNode>, panel_id: &str) -> bool {
    let Some(node) = node else {
        return false;
    };
    match node {
        LayoutNode::Split { first, second, .. } if matches!(first.as_ref(), LayoutNode::Panel { panel } if panel.id == panel_id) =>
        {
            *node = (**second).clone();
            true
        }
        LayoutNode::Split { first, second, .. } if matches!(second.as_ref(), LayoutNode::Panel { panel } if panel.id == panel_id) =>
        {
            *node = (**first).clone();
            true
        }
        LayoutNode::Split { first, second, .. } => {
            remove_panel(Some(first), panel_id) || remove_panel(Some(second), panel_id)
        }
        LayoutNode::Panel { .. } => false,
    }
}

fn split_panel(
    node: Option<&mut LayoutNode>,
    panel_id: &str,
    axis: SplitAxis,
    fraction: f32,
    split_id: u64,
    new_panel: PanelState,
) -> bool {
    let Some(node) = node else {
        return false;
    };
    match node {
        LayoutNode::Panel { panel } if panel.id == panel_id => {
            let existing = LayoutNode::Panel {
                panel: panel.clone(),
            };
            *node = LayoutNode::Split {
                id: split_id,
                axis,
                fraction: fraction.clamp(0.1, 0.9),
                first: Box::new(existing),
                second: Box::new(LayoutNode::Panel { panel: new_panel }),
            };
            true
        }
        LayoutNode::Split { first, second, .. } => {
            split_panel(
                Some(first),
                panel_id,
                axis,
                fraction,
                split_id,
                new_panel.clone(),
            ) || split_panel(Some(second), panel_id, axis, fraction, split_id, new_panel)
        }
        LayoutNode::Panel { .. } => false,
    }
}

#[derive(Debug, Clone, Copy)]
enum PanelControlIcon {
    Maximize,
    RestoreLayout,
}

struct PanelContentButton<'a> {
    spec: PanelSpec<'a>,
    selected: bool,
}

fn panel_content_button(spec: PanelSpec<'_>, selected: bool) -> PanelContentButton<'_> {
    PanelContentButton { spec, selected }
}

impl egui::Widget for PanelContentButton<'_> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let response = ui.add_sized(
            [190.0, 24.0],
            egui::Button::selectable(
                self.selected,
                egui::RichText::new(self.spec.title).color(Color32::TRANSPARENT),
            ),
        );
        let color = ui.visuals().widgets.style(&response).fg_stroke.color;
        let icon_rect = Rect::from_center_size(
            egui::pos2(response.rect.left() + 14.0, response.rect.center().y),
            egui::vec2(16.0, 16.0),
        );
        self.spec.icon.paint(ui, icon_rect, color);
        ui.painter().text(
            egui::pos2(response.rect.left() + 28.0, response.rect.center().y),
            egui::Align2::LEFT_CENTER,
            self.spec.title,
            egui::TextStyle::Button.resolve(ui.style()),
            color,
        );
        response
    }
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
        PanelControlIcon::Maximize => {
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
            ui.painter()
                .hline(center.x - 4.0..=center.x + 4.0, center.y, stroke);
            for direction in [-1.0, 1.0] {
                let outside = center.y + direction * 5.0;
                let inside = center.y + direction;
                ui.painter().line_segment(
                    [egui::pos2(center.x, outside), egui::pos2(center.x, inside)],
                    stroke,
                );
                ui.painter().line_segment(
                    [
                        egui::pos2(center.x - 2.0, center.y + direction * 3.0),
                        egui::pos2(center.x, inside),
                    ],
                    stroke,
                );
                ui.painter().line_segment(
                    [
                        egui::pos2(center.x + 2.0, center.y + direction * 3.0),
                        egui::pos2(center.x, inside),
                    ],
                    stroke,
                );
            }
        }
    }
    response.on_hover_text(tooltip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs() -> [PanelSpec<'static>; 5] {
        [
            PanelSpec::new("viewer", "Viewer", 100.0).singleton(),
            PanelSpec::new("graph", "Graph", 100.0).singleton(),
            PanelSpec::new("watches", "Watches", 80.0),
            PanelSpec::new("triggers", "Triggers", 80.0),
            PanelSpec::new("decoder", "Decoder", 80.0).icon(PanelIcon::Table),
        ]
    }

    #[test]
    fn initial_vertical_layout_preserves_requested_fraction() {
        let layout = PanelLayout::new([("viewer", 0.25), ("graph", 0.75)]);
        assert_eq!(layout.split_fraction("viewer", "graph"), Some(0.25));
    }

    #[test]
    fn panel_icons_are_explicit_spec_metadata() {
        assert_eq!(
            PanelSpec::new("plain", "Plain", 100.0).icon,
            PanelIcon::Panel
        );
        assert_eq!(
            PanelSpec::new("signal", "Signal", 100.0)
                .icon(PanelIcon::Waveform)
                .icon,
            PanelIcon::Waveform
        );
    }

    #[test]
    fn title_interaction_excludes_content_and_control_columns() {
        let title = Rect::from_min_max(egui::pos2(0.0, 10.0), egui::pos2(400.0, 42.0));
        let interaction = title_interaction_rect(title, 135.0, 370.0).unwrap();

        assert_eq!(interaction.left(), 135.0);
        assert_eq!(interaction.right(), 370.0);
        assert!(!interaction.contains(egui::pos2(100.0, 20.0)));
        assert!(!interaction.contains(egui::pos2(390.0, 20.0)));
        assert!(interaction.contains(egui::pos2(250.0, 20.0)));
        assert!(title_interaction_rect(title, 380.0, 370.0).is_none());
    }

    #[test]
    fn rendered_title_interaction_avoids_host_widgets_and_panel_controls() {
        let context = egui::Context::default();
        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 300.0));
        context.begin_pass(egui::RawInput {
            screen_rect: Some(rect),
            ..Default::default()
        });
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("title-interaction-test"),
            UiBuilder::new().max_rect(rect),
        );
        let mut layout = PanelLayout::new([("viewer", 1.0)]);

        let response = layout.show(&mut ui, rect, 0.0, &specs(), |slot, ui| {
            if matches!(slot, PanelSlot::TitleBar { .. }) {
                ui.label("Capture status");
                let _ = ui.small_button("Stop");
            }
        });
        let _ = context.end_pass();
        let panel = response.panel("viewer").unwrap();
        let interaction = panel.title_interaction_rect.unwrap();

        assert!(interaction.left() > panel.title_rect.left() + 44.0);
        assert!(interaction.right() < panel.title_rect.right());
        assert!(interaction.width() > 0.0);
    }

    #[test]
    fn splitting_and_joining_mutates_only_the_split_tree() {
        let mut layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
        layout.apply_action(
            LayoutAction::Split {
                panel_id: "viewer".to_owned(),
                axis: SplitAxis::Vertical,
                fraction: 0.3,
            },
            &specs(),
        );
        let root = layout.state.root.as_ref().unwrap();
        let LayoutNode::Split { first, .. } = root else {
            panic!("expected initial split");
        };
        let LayoutNode::Split { id, fraction, .. } = first.as_ref() else {
            panic!("expected nested split");
        };
        assert_eq!(*fraction, 0.3);
        let nested_id = *id;
        assert_eq!(all_panels(layout.state.root.as_ref()).len(), 3);

        layout.apply_action(
            LayoutAction::Join {
                split_id: nested_id,
                keep: SplitSide::First,
            },
            &specs(),
        );
        assert_eq!(all_panels(layout.state.root.as_ref()).len(), 2);
        assert!(find_panel_by_content(layout.state.root.as_ref(), "viewer").is_some());
    }

    #[test]
    fn join_labels_describe_the_surviving_panels_expansion_direction() {
        assert_eq!(
            join_options(SplitAxis::Vertical),
            (
                ("Join Left", SplitSide::Second),
                ("Join Right", SplitSide::First),
            )
        );
        assert_eq!(
            join_options(SplitAxis::Horizontal),
            (
                ("Join Up", SplitSide::Second),
                ("Join Down", SplitSide::First),
            )
        );
    }

    #[test]
    fn singleton_content_cannot_be_assigned_to_a_new_split() {
        let mut layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
        layout.apply_action(
            LayoutAction::Split {
                panel_id: "viewer".to_owned(),
                axis: SplitAxis::Horizontal,
                fraction: 0.5,
            },
            &specs(),
        );
        let contents: Vec<_> = all_panels(layout.state.root.as_ref())
            .into_iter()
            .map(|panel| panel.content.as_str())
            .collect();
        assert_eq!(contents, ["viewer", "watches", "graph"]);
    }

    #[test]
    fn split_target_follows_pointer_across_all_visible_panels() {
        let layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let (panels, _) = layout.geometries(rect, &specs());
        let viewer = panels
            .iter()
            .find(|panel| panel.content_id == "viewer")
            .unwrap();
        let graph = panels
            .iter()
            .find(|panel| panel.content_id == "graph")
            .unwrap();

        assert_eq!(
            panel_at_pointer(&panels, viewer.panel_rect.center())
                .map(|panel| panel.content_id.as_str()),
            Some("viewer")
        );
        assert_eq!(
            panel_at_pointer(&panels, graph.panel_rect.center())
                .map(|panel| panel.content_id.as_str()),
            Some("graph")
        );
    }

    #[test]
    fn live_split_preview_renders_final_geometry_without_committing_state() {
        let layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let (preview, boundaries) = layout.split_preview_geometries(
            rect,
            &specs(),
            LayoutAction::Split {
                panel_id: "viewer".to_owned(),
                axis: SplitAxis::Vertical,
                fraction: 0.3,
            },
        );

        assert_eq!(preview.len(), 3);
        assert_eq!(boundaries.len(), 2);
        assert!(preview.iter().any(|panel| panel.content_id == "watches"));
        assert_eq!(all_panels(layout.state.root.as_ref()).len(), 2);
    }

    #[test]
    fn adding_an_area_to_the_layout_wraps_the_complete_existing_tree() {
        for (side, fraction, expected_axis) in [
            (LayoutSide::Left, 0.25, SplitAxis::Vertical),
            (LayoutSide::Right, 0.75, SplitAxis::Vertical),
            (LayoutSide::Top, 0.25, SplitAxis::Horizontal),
            (LayoutSide::Bottom, 0.75, SplitAxis::Horizontal),
        ] {
            let mut layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
            layout.apply_action(LayoutAction::SplitLayout { side, fraction }, &specs());

            let LayoutNode::Split {
                axis,
                fraction: actual_fraction,
                first,
                second,
                ..
            } = layout.state.root.as_ref().unwrap()
            else {
                panic!("expected a new root split");
            };
            assert_eq!(*axis, expected_axis);
            assert_eq!(*actual_fraction, fraction);
            let (new_area, previous_layout) = if side.new_area_is_first() {
                (first.as_ref(), second.as_ref())
            } else {
                (second.as_ref(), first.as_ref())
            };
            assert!(matches!(new_area, LayoutNode::Panel { .. }));
            assert!(contains_content(previous_layout, "viewer"));
            assert!(contains_content(previous_layout, "graph"));
        }
    }

    #[test]
    fn full_height_side_area_preview_matches_the_committed_layout() {
        let layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let action = LayoutAction::SplitLayout {
            side: LayoutSide::Right,
            fraction: 0.75,
        };
        let (preview, preview_boundaries) =
            layout.split_preview_geometries(rect, &specs(), action.clone());
        let side_panel = preview
            .iter()
            .find(|panel| panel.content_id == "watches")
            .unwrap();

        assert_eq!(side_panel.panel_rect.top(), rect.top());
        assert_eq!(side_panel.panel_rect.bottom(), rect.bottom());
        assert_eq!(preview.len(), 3);
        assert_eq!(preview_boundaries.len(), 2);
        assert_eq!(all_panels(layout.state.root.as_ref()).len(), 2);

        let mut committed = layout;
        committed.apply_action(action, &specs());
        let (committed_panels, committed_boundaries) = committed.geometries(rect, &specs());
        assert_eq!(committed_panels.len(), preview.len());
        assert_eq!(committed_boundaries.len(), preview_boundaries.len());
    }

    #[test]
    fn right_column_content_is_added_once_and_kept_in_declared_order() {
        for requested in [
            ["watches", "triggers", "decoder"],
            ["decoder", "triggers", "watches"],
        ] {
            let mut layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
            for content in requested {
                assert!(layout.ensure_right_column_content(
                    content,
                    &["watches", "triggers", "decoder"],
                    0.75,
                ));
            }
            assert!(!layout.ensure_right_column_content(
                "watches",
                &["watches", "triggers", "decoder"],
                0.75,
            ));

            let LayoutNode::Split {
                axis: SplitAxis::Vertical,
                first,
                second,
                ..
            } = layout.state.root.as_ref().unwrap()
            else {
                panic!("expected a right-side column");
            };
            assert!(contains_content(first, "viewer"));
            assert!(contains_content(first, "graph"));
            let column_contents: Vec<_> = all_panels(Some(second))
                .into_iter()
                .map(|panel| panel.content.as_str())
                .collect();
            assert_eq!(column_contents, ["watches", "triggers", "decoder"]);
            assert_eq!(all_panels(layout.state.root.as_ref()).len(), 5);
        }
    }

    #[test]
    fn right_column_can_contain_multiple_instances_of_one_content() {
        let mut layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);

        assert!(layout.ensure_right_column_content_count(
            "decoder",
            2,
            &["watches", "triggers", "decoder"],
            0.75,
        ));
        assert!(!layout.ensure_right_column_content_count(
            "decoder",
            2,
            &["watches", "triggers", "decoder"],
            0.75,
        ));

        let decoder_count = all_panels(layout.state.root.as_ref())
            .into_iter()
            .filter(|panel| panel.content == "decoder")
            .count();
        assert_eq!(decoder_count, 2);
    }

    #[test]
    fn first_right_column_content_spans_the_complete_layout_height() {
        let mut layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
        assert!(layout.ensure_right_column_content("watches", &["watches", "triggers"], 0.75,));
        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let (panels, _) = layout.geometries(rect, &specs());
        let watches = panels
            .iter()
            .find(|panel| panel.content_id == "watches")
            .unwrap();

        assert_eq!(watches.panel_rect.top(), rect.top());
        assert_eq!(watches.panel_rect.bottom(), rect.bottom());
    }

    #[test]
    fn maximizing_and_restoring_preserves_the_split_layout() {
        let mut layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
        layout.apply_panel_action("graph", PanelAction::Maximize);
        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let (maximized, _) = layout.geometries(rect, &specs());
        assert_eq!(maximized.len(), 1);
        assert_eq!(maximized[0].content_id, "graph");

        layout.apply_panel_action("graph", PanelAction::RestoreMaximized);
        let (restored, _) = layout.geometries(rect, &specs());
        assert_eq!(restored.len(), 2);
    }

    #[test]
    fn control_drag_aligns_a_boundary_with_a_parallel_neighbour() {
        let root = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let dragged = BoundaryGeometry {
            id: 2,
            axis: SplitAxis::Horizontal,
            rect: Rect::from_min_size(egui::pos2(600.0, 398.0), egui::vec2(200.0, 4.0)),
            parent_rect: Rect::from_min_size(egui::pos2(600.0, 0.0), egui::vec2(200.0, 600.0)),
        };
        let neighbour = BoundaryGeometry {
            id: 1,
            axis: SplitAxis::Horizontal,
            rect: Rect::from_min_size(egui::pos2(0.0, 298.0), egui::vec2(600.0, 4.0)),
            parent_rect: Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 600.0)),
        };
        let boundaries = [dragged.clone(), neighbour];

        let fraction = dragged.snapped_fraction_at(egui::pos2(700.0, 307.0), &boundaries, root);
        let (_, splitter, _) = split_rects(
            dragged.parent_rect,
            SplitAxis::Horizontal,
            fraction,
            egui::Vec2::ZERO,
            egui::Vec2::ZERO,
            4.0,
        );

        assert_eq!(splitter.center().y, 300.0);
    }

    #[test]
    fn control_drag_uses_the_layout_grid_away_from_neighbour_boundaries() {
        let root = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let dragged = BoundaryGeometry {
            id: 2,
            axis: SplitAxis::Horizontal,
            rect: Rect::from_min_size(egui::pos2(600.0, 398.0), egui::vec2(200.0, 4.0)),
            parent_rect: Rect::from_min_size(egui::pos2(600.0, 0.0), egui::vec2(200.0, 600.0)),
        };

        let fraction = dragged.snapped_fraction_at(
            egui::pos2(700.0, 333.0),
            std::slice::from_ref(&dragged),
            root,
        );
        let (_, splitter, _) = split_rects(
            dragged.parent_rect,
            SplitAxis::Horizontal,
            fraction,
            egui::Vec2::ZERO,
            egui::Vec2::ZERO,
            4.0,
        );

        assert_eq!(splitter.center().y, 336.0);
    }

    #[test]
    fn shift_drag_lines_up_nearby_parallel_boundaries() {
        let root = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let dragged = BoundaryGeometry {
            id: 1,
            axis: SplitAxis::Horizontal,
            rect: Rect::from_min_size(egui::pos2(0.0, 298.0), egui::vec2(400.0, 4.0)),
            parent_rect: Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 600.0)),
        };
        let neighbour = BoundaryGeometry {
            id: 2,
            axis: SplitAxis::Horizontal,
            rect: Rect::from_min_size(egui::pos2(400.0, 306.0), egui::vec2(400.0, 4.0)),
            parent_rect: Rect::from_min_size(egui::pos2(400.0, 0.0), egui::vec2(400.0, 600.0)),
        };
        let boundaries = [dragged.clone(), neighbour.clone()];

        let actions =
            dragged.resize_actions(egui::pos2(200.0, 350.0), &boundaries, root, false, true);
        let fractions: Vec<_> = actions
            .iter()
            .map(|action| match action {
                LayoutAction::SetFraction { split_id, fraction } => (*split_id, *fraction),
                _ => panic!("resize emitted a non-resize action"),
            })
            .collect();

        assert_eq!(fractions.len(), 2);
        assert_eq!(fractions[0].0, dragged.id);
        assert_eq!(fractions[1].0, neighbour.id);
        assert!((dragged.coordinate_for_fraction(fractions[0].1) - 350.0).abs() < 0.001);
        assert!((neighbour.coordinate_for_fraction(fractions[1].1) - 350.0).abs() < 0.001);
    }

    #[test]
    fn option_break_transposes_an_aligned_grid_and_keeps_the_dragged_segment_id() {
        let panel = |id: &str| LayoutNode::Panel {
            panel: PanelState {
                id: id.to_owned(),
                content: id.to_owned(),
                title_bar_position: TitleBarPosition::Top,
            },
        };
        let mut root = LayoutNode::Split {
            id: 1,
            axis: SplitAxis::Vertical,
            fraction: 0.4,
            first: Box::new(LayoutNode::Split {
                id: 2,
                axis: SplitAxis::Horizontal,
                fraction: 0.5,
                first: Box::new(panel("top-left")),
                second: Box::new(panel("bottom-left")),
            }),
            second: Box::new(LayoutNode::Split {
                id: 3,
                axis: SplitAxis::Horizontal,
                fraction: 0.5,
                first: Box::new(panel("top-right")),
                second: Box::new(panel("bottom-right")),
            }),
        };

        assert!(break_split(Some(&mut root), 1, SplitSide::First, 0.5));

        let LayoutNode::Split {
            id,
            axis,
            first,
            second,
            ..
        } = root
        else {
            panic!("broken grid root is not a split");
        };
        assert_eq!((id, axis), (2, SplitAxis::Horizontal));
        assert!(matches!(
            first.as_ref(),
            LayoutNode::Split {
                id: 1,
                axis: SplitAxis::Vertical,
                ..
            }
        ));
        assert!(matches!(
            second.as_ref(),
            LayoutNode::Split {
                id: 3,
                axis: SplitAxis::Vertical,
                ..
            }
        ));
        let rebuilt = LayoutNode::Split {
            id,
            axis,
            fraction: 0.5,
            first,
            second,
        };
        let contents: Vec<_> = all_panels(Some(&rebuilt))
            .into_iter()
            .map(|panel| panel.content.as_str())
            .collect();
        assert_eq!(
            contents,
            ["top-left", "top-right", "bottom-left", "bottom-right"]
        );
    }

    #[test]
    fn area_menu_uses_shared_shortcut_formatter() {
        for (key, expected) in [(egui::Key::Space, "^ Space"), (egui::Key::A, "^ A")] {
            let shortcut = KeyboardShortcut::new(egui::Modifiers::CTRL, key);
            assert_eq!(MenuShortcut::from_keyboard(shortcut).to_string(), expected);
        }
    }

    #[test]
    fn configured_shortcut_maximizes_hovered_area_and_then_restores() {
        fn press_shortcut(layout: &mut PanelLayout) {
            let context = egui::Context::default();
            let modifiers = egui::Modifiers::CTRL;
            let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
            context.begin_pass(egui::RawInput {
                screen_rect: Some(rect),
                modifiers,
                events: vec![
                    egui::Event::PointerMoved(egui::pos2(100.0, 100.0)),
                    egui::Event::Key {
                        key: egui::Key::Space,
                        physical_key: Some(egui::Key::Space),
                        pressed: true,
                        repeat: false,
                        modifiers,
                    },
                ],
                ..Default::default()
            });
            let mut ui = egui::Ui::new(
                context.clone(),
                egui::Id::new("panel-shortcut-test"),
                UiBuilder::new().max_rect(rect),
            );
            layout.set_maximize_shortcut(Some(KeyboardShortcut::new(modifiers, egui::Key::Space)));
            layout.show(&mut ui, rect, 0.0, &specs(), |_, _| {});
            let _ = context.end_pass();
        }

        let mut layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
        press_shortcut(&mut layout);
        assert_eq!(layout.state.maximized.as_deref(), Some("viewer"));

        press_shortcut(&mut layout);
        assert_eq!(layout.state.maximized, None);
    }

    #[test]
    fn title_bar_can_be_flipped_to_the_bottom() {
        let mut layout = PanelLayout::new([("viewer", 1.0)]);
        layout.apply_panel_action("viewer", PanelAction::FlipTitleBar);
        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let (panels, _) = layout.geometries(rect, &specs());
        let panel = &panels[0];

        assert_eq!(panel.title_bar_position, TitleBarPosition::Bottom);
        assert_eq!(panel.title_rect.bottom(), panel.panel_rect.bottom());
        assert_eq!(panel.body_rect.bottom(), panel.title_rect.top());
    }

    #[test]
    fn closing_an_area_expands_its_sibling() {
        let mut layout = PanelLayout::new([("viewer", 0.5), ("graph", 0.5)]);
        layout.apply_panel_action("viewer", PanelAction::Close);

        let panels = all_panels(layout.state.root.as_ref());
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0].content, "graph");

        layout.apply_panel_action("graph", PanelAction::Close);
        assert_eq!(all_panels(layout.state.root.as_ref()).len(), 1);
    }

    #[test]
    fn legacy_minimized_state_loads_as_a_regular_panel() {
        let json = r#"{
            "root": {
                "kind": "panel",
                "panel": {"id": "viewer", "content": "viewer", "minimized": true}
            },
            "maximized": null,
            "restore_minimized": [["viewer", true]],
            "next_id": 1
        }"#;
        let restored: PanelLayoutState = serde_json::from_str(json).unwrap();
        let panel = find_panel(restored.root.as_ref(), "viewer").unwrap();

        assert_eq!(panel.title_bar_position, TitleBarPosition::Top);
        let serialized = serde_json::to_string(&restored).unwrap();
        assert!(!serialized.contains("minimized"));
    }

    #[test]
    fn state_round_trips_nested_layout() {
        let mut layout = PanelLayout::new([("viewer", 0.4), ("graph", 0.6)]);
        layout.apply_action(
            LayoutAction::Split {
                panel_id: "graph".to_owned(),
                axis: SplitAxis::Vertical,
                fraction: 0.7,
            },
            &specs(),
        );
        let json = serde_json::to_string(layout.state()).unwrap();
        let restored: PanelLayoutState = serde_json::from_str(&json).unwrap();
        assert_eq!(all_panels(restored.root.as_ref()).len(), 3);
    }
}
