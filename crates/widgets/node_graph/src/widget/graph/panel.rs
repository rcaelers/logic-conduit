//! Blender-style properties panel (N-panel): a resizable strip docked to the
//! right border of the graph view. The Node tab shows the *active* node's
//! low-frequency configuration; the View tab shows its viewer lane and
//! presentation configuration. Widgets render in screen space at full
//! size, unaffected by graph zoom; edits mutate the same node state as
//! inline controls and run `on_update` through the same path.

use egui::{
    Align, Align2, Color32, CursorIcon, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui,
    UiBuilder, Vec2,
};

use super::widget::NodeGraphWidget;
use crate::model::{NodeId, NodeKind};

const PANEL_MIN_WIDTH: f32 = 220.0;
const PANEL_MAX_WIDTH: f32 = 520.0;
const TAB_BAR_WIDTH: f32 = 24.0;
const TAB_HEIGHT: f32 = 70.0;
const DEFAULT_ROW_HEIGHT: f32 = 24.0;
const PANEL_MARGIN_Y: f32 = 8.0;
const PANEL_TITLE_BLOCK_HEIGHT: f32 = 44.0;
const COLLAPSING_HEADER_HEIGHT: f32 = 26.0;
const PANEL_SECTION_GAP: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PanelTab {
    Node,
    View,
}

pub(crate) struct PanelState {
    pub active_tab: Option<PanelTab>,
    pub width: f32,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            active_tab: Some(PanelTab::Node),
            width: 300.0,
        }
    }
}

impl NodeGraphWidget {
    pub(crate) fn toggle_panel(&mut self) {
        self.toggle_panel_tab(PanelTab::Node);
    }

    fn toggle_panel_tab(&mut self, tab: PanelTab) {
        self.panel.active_tab = if self.panel.active_tab == Some(tab) {
            None
        } else {
            Some(tab)
        };
    }

    /// The node the panel shows: the active (most recently clicked/added)
    /// regular node if it still exists, otherwise the newest regular selected
    /// node. Canvas deselection does not clear the active node; the side tab
    /// owns panel visibility.
    fn panel_target(&self) -> Option<NodeId> {
        let shown = |id: &NodeId| {
            self.graph
                .nodes
                .get(id)
                .is_some_and(|node| node.kind == NodeKind::Regular)
                && self.runtime.contains_key(id)
        };
        self.active_node.filter(shown).or_else(|| {
            self.graph
                .nodes
                .keys()
                .filter(|id| {
                    shown(id) && self.graph.nodes.get(id).is_some_and(|node| node.selected)
                })
                .max_by_key(|id| id.0)
                .copied()
        })
    }

    /// Screen rect occupied by the always-visible right-side tab strip.
    pub(crate) fn panel_tab_bar_rect(&self, canvas_rect: Rect) -> Rect {
        Rect::from_min_max(
            Pos2::new(canvas_rect.max.x - TAB_BAR_WIDTH, canvas_rect.min.y),
            canvas_rect.max,
        )
    }

    /// Screen rect the panel occupies this frame, `None` while hidden.
    pub(crate) fn panel_rect(&self, canvas_rect: Rect) -> Option<Rect> {
        self.panel.active_tab?;
        let width = self.panel.width.clamp(
            PANEL_MIN_WIDTH,
            (canvas_rect.width() - TAB_BAR_WIDTH - 160.0).max(PANEL_MIN_WIDTH),
        );
        let height = self.panel_height(canvas_rect);
        let tab_bar = self.panel_tab_bar_rect(canvas_rect);
        Some(Rect::from_min_max(
            Pos2::new(tab_bar.left() - width, canvas_rect.min.y),
            Pos2::new(tab_bar.left(), canvas_rect.min.y + height),
        ))
    }

    fn panel_height(&self, canvas_rect: Rect) -> f32 {
        let natural = match self.panel.active_tab {
            Some(PanelTab::Node) => self.node_panel_height(),
            Some(PanelTab::View) => self.view_panel_height(),
            None => 0.0,
        };
        natural.clamp(0.0, canvas_rect.height().max(0.0))
    }

    fn node_panel_height(&self) -> f32 {
        let Some(node_id) = self.panel_target() else {
            return PANEL_MARGIN_Y * 2.0 + PANEL_TITLE_BLOCK_HEIGHT;
        };
        let mut height = PANEL_MARGIN_Y * 2.0
            + PANEL_TITLE_BLOCK_HEIGHT
            + COLLAPSING_HEADER_HEIGHT
            + 2.0 * DEFAULT_ROW_HEIGHT
            + PANEL_SECTION_GAP;

        if let Some(instance) = self.runtime.get(&node_id) {
            for section in instance.panel_sections() {
                height += COLLAPSING_HEADER_HEIGHT + PANEL_SECTION_GAP;
                height += section
                    .props
                    .iter()
                    .map(|prop| prop.height.unwrap_or(DEFAULT_ROW_HEIGHT))
                    .sum::<f32>();
            }
        }

        height
    }

    fn view_panel_height(&self) -> f32 {
        let Some(node_id) = self.panel_target() else {
            return PANEL_MARGIN_Y * 2.0 + PANEL_TITLE_BLOCK_HEIGHT;
        };
        let mut height = PANEL_MARGIN_Y * 2.0 + PANEL_TITLE_BLOCK_HEIGHT;

        let watchable_outputs = self
            .graph
            .nodes
            .get(&node_id)
            .map(|node| {
                node.outputs
                    .iter()
                    .filter(|output| output.visible && output.view_selectable)
                    .count()
            })
            .unwrap_or(0);
        if watchable_outputs > 0 {
            height += COLLAPSING_HEADER_HEIGHT
                + PANEL_SECTION_GAP
                + watchable_outputs as f32 * DEFAULT_ROW_HEIGHT;
        }

        if let Some(instance) = self.runtime.get(&node_id) {
            for section in instance.view_panel_sections() {
                height += COLLAPSING_HEADER_HEIGHT + PANEL_SECTION_GAP;
                height += section
                    .props
                    .iter()
                    .map(|prop| prop.height.unwrap_or(DEFAULT_ROW_HEIGHT))
                    .sum::<f32>();
            }
        }

        height
    }

    /// Allocates the panel splitter. The body itself deliberately has no
    /// parent interaction response: its scroll area and child widgets own
    /// their pointer input, while `graph_pointer` keeps canvas gestures out.
    pub(crate) fn update_panel_interaction(&mut self, ui: &mut Ui, panel_rect: Rect) {
        let splitter_rect = Rect::from_min_max(
            Pos2::new(panel_rect.left() - 3.0, panel_rect.top()),
            Pos2::new(panel_rect.left() + 3.0, panel_rect.bottom()),
        );
        let splitter = ui.interact(
            splitter_rect,
            ui.id().with("props-panel-splitter"),
            Sense::click_and_drag(),
        );
        if splitter.hovered() || splitter.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if splitter.dragged()
            && let Some(pointer) = splitter.interact_pointer_pos()
        {
            self.panel.width =
                (panel_rect.right() - pointer.x).clamp(PANEL_MIN_WIDTH, PANEL_MAX_WIDTH);
        }
    }

    pub(crate) fn update_panel_tab_bar_interaction(&mut self, ui: &mut Ui, tab_bar_rect: Rect) {
        for tab in [PanelTab::Node, PanelTab::View] {
            let response = ui.interact(
                self.panel_tab_rect(tab_bar_rect, tab),
                ui.id().with(("props-panel-tab", tab)),
                Sense::click(),
            );
            if response.clicked() {
                self.toggle_panel_tab(tab);
            }
        }
    }

    pub(crate) fn show_panel_tab_bar(&self, ui: &mut Ui, tab_bar_rect: Rect) {
        let painter = ui.painter_at(tab_bar_rect);
        painter.rect_filled(tab_bar_rect, 0.0, Color32::from_rgb(31, 31, 31));
        painter.line_segment(
            [tab_bar_rect.left_top(), tab_bar_rect.left_bottom()],
            Stroke::new(1.0, Color32::from_rgb(62, 62, 62)),
        );

        for tab in [PanelTab::Node, PanelTab::View] {
            let rect = self.panel_tab_rect(tab_bar_rect, tab);
            let active = self.panel.active_tab == Some(tab);
            let fill = if active {
                Color32::from_rgb(58, 58, 58)
            } else {
                Color32::from_rgb(39, 39, 39)
            };
            let stroke = if active {
                Color32::from_rgb(92, 92, 92)
            } else {
                Color32::from_rgb(55, 55, 55)
            };
            painter.rect_filled(rect.shrink(1.0), 4.0, fill);
            painter.rect_stroke(
                rect.shrink(1.0),
                4.0,
                Stroke::new(1.0, stroke),
                egui::StrokeKind::Inside,
            );

            let text = match tab {
                PanelTab::Node => "Node",
                PanelTab::View => "View",
            };
            let color = if active {
                Color32::WHITE
            } else {
                Color32::from_rgb(185, 185, 185)
            };
            let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(12.0), color);
            let text_pos = rect.center() - galley.rect.center().to_vec2();
            let mut shape = egui::epaint::TextShape::new(text_pos, galley, color)
                .with_angle_and_anchor(-std::f32::consts::FRAC_PI_2, Align2::CENTER_CENTER);
            shape.override_text_color = Some(color);
            painter.add(shape);
        }
    }

    fn panel_tab_rect(&self, tab_bar_rect: Rect, tab: PanelTab) -> Rect {
        let top = tab_bar_rect.top()
            + 8.0
            + match tab {
                PanelTab::Node => 0.0,
                PanelTab::View => TAB_HEIGHT + 6.0,
            };
        Rect::from_min_size(
            Pos2::new(tab_bar_rect.left(), top),
            Vec2::new(tab_bar_rect.width(), TAB_HEIGHT),
        )
    }

    pub(crate) fn show_active_panel(&mut self, ui: &mut Ui, panel_rect: Rect) {
        match self.panel.active_tab {
            Some(PanelTab::Node) => self.show_properties_panel(ui, panel_rect),
            Some(PanelTab::View) => self.show_view_panel(ui, panel_rect),
            None => {}
        }
    }

    fn show_properties_panel(&mut self, ui: &mut Ui, panel_rect: Rect) {
        let Some(node_id) = self.panel_target() else {
            self.show_empty_node_panel(ui, panel_rect);
            return;
        };

        let painter = ui.painter_at(panel_rect);
        painter.rect_filled(panel_rect, 0.0, Color32::from_rgb(38, 38, 38));
        painter.line_segment(
            [panel_rect.left_top(), panel_rect.left_bottom()],
            Stroke::new(1.0_f32, Color32::from_rgb(70, 70, 70)),
        );

        let Some(node) = self.graph.nodes.get_mut(&node_id) else {
            return;
        };
        let Some(instance) = self.runtime.get_mut(&node_id) else {
            return;
        };
        let category = self
            .registry
            .category_of(node.def_name())
            .unwrap_or("")
            .to_owned();
        let sections = instance.panel_sections();
        let editing_enabled = self.editing_enabled;

        let content = panel_rect.shrink2(Vec2::new(10.0, 8.0));
        let mut changed = false;
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(content)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                ui.set_clip_rect(panel_rect);
                egui::ScrollArea::vertical()
                    .id_salt("props-panel-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.push_id(("props-panel", node_id.0), |ui| {
                            ui.label(RichText::new(&node.title).size(15.0).strong());
                            ui.label(
                                RichText::new(format!("{} · {}", node.def_name(), category))
                                    .size(11.0)
                                    .weak(),
                            );
                            ui.add_space(6.0);

                            // Built-in section: identity of the node itself.
                            egui::CollapsingHeader::new("Node")
                                .default_open(true)
                                .show(ui, |ui| {
                                    ui.add_enabled_ui(editing_enabled, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new("Name").size(11.0));
                                            ui.text_edit_singleline(&mut node.title);
                                        });
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new("Color").size(11.0));
                                            ui.color_edit_button_srgba(&mut node.header_color);
                                        });
                                    });
                                });

                            for (section_index, section) in sections.iter().enumerate() {
                                egui::CollapsingHeader::new(section.title)
                                    .id_salt(("props-panel-section", section.title, section_index))
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        for (prop_index, prop) in section.props.iter().enumerate() {
                                            ui.push_id(
                                                (
                                                    "props-panel-property",
                                                    section.title,
                                                    section_index,
                                                    prop.id,
                                                ),
                                                |ui| {
                                                    let height =
                                                        prop.height.unwrap_or(DEFAULT_ROW_HEIGHT);
                                                    let width = ui.available_width();
                                                    let (rect, _) = ui.allocate_exact_size(
                                                        Vec2::new(width, height),
                                                        Sense::hover(),
                                                    );
                                                    if ui
                                                        .add_enabled_ui(editing_enabled, |ui| {
                                                            instance.draw_panel_prop(
                                                                section_index,
                                                                prop_index,
                                                                ui,
                                                                rect,
                                                                panel_rect,
                                                            )
                                                        })
                                                        .inner
                                                    {
                                                        changed = true;
                                                    }
                                                },
                                            );
                                        }
                                    });
                            }
                        });
                    });
            },
        );

        if changed {
            self.run_update(node_id);
        }
    }

    fn show_empty_node_panel(&self, ui: &mut Ui, panel_rect: Rect) {
        let painter = ui.painter_at(panel_rect);
        painter.rect_filled(panel_rect, 0.0, Color32::from_rgb(38, 38, 38));
        painter.line_segment(
            [panel_rect.left_top(), panel_rect.left_bottom()],
            Stroke::new(1.0_f32, Color32::from_rgb(70, 70, 70)),
        );
        let content = panel_rect.shrink2(Vec2::new(10.0, 8.0));
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(content)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                ui.set_clip_rect(panel_rect);
                ui.label(RichText::new("Node").size(15.0).strong());
                ui.label(RichText::new("No active node").size(11.0).weak());
            },
        );
    }

    fn show_view_panel(&mut self, ui: &mut Ui, panel_rect: Rect) {
        let Some(node_id) = self.panel_target() else {
            self.show_empty_view_panel(ui, panel_rect);
            return;
        };

        let painter = ui.painter_at(panel_rect);
        painter.rect_filled(panel_rect, 0.0, Color32::from_rgb(38, 38, 38));
        painter.line_segment(
            [panel_rect.left_top(), panel_rect.left_bottom()],
            Stroke::new(1.0_f32, Color32::from_rgb(70, 70, 70)),
        );
        let Some(node) = self.graph.nodes.get_mut(&node_id) else {
            return;
        };
        let Some(instance) = self.runtime.get_mut(&node_id) else {
            return;
        };
        let sections = instance.view_panel_sections();
        let editing_enabled = self.editing_enabled;

        let content = panel_rect.shrink2(Vec2::new(10.0, 8.0));
        let mut changed = false;
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(content)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                ui.set_clip_rect(panel_rect);
                egui::ScrollArea::vertical()
                    .id_salt("view-panel-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.push_id(("view-panel", node_id.0), |ui| {
                            ui.label(RichText::new(&node.title).size(15.0).strong());
                            ui.label(RichText::new("Viewer settings").size(11.0).weak());
                            ui.add_space(6.0);

                            // Generic lane selection: one checkbox per visible
                            // output. The compiler consumes `show_in_view`
                            // without interpreting the node or output names.
                            let watchable: Vec<usize> = node
                                .outputs
                                .iter()
                                .enumerate()
                                .filter(|(_, output)| output.visible && output.view_selectable)
                                .map(|(index, _)| index)
                                .collect();
                            if !watchable.is_empty() {
                                egui::CollapsingHeader::new("Lanes")
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        for index in watchable {
                                            let output = &mut node.outputs[index];
                                            if ui
                                                .add_enabled(
                                                    editing_enabled,
                                                    egui::Checkbox::new(
                                                        &mut output.show_in_view,
                                                        &output.name,
                                                    ),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                        }
                                    });
                            }

                            for (section_index, section) in sections.iter().enumerate() {
                                egui::CollapsingHeader::new(section.title)
                                    .id_salt(("view-panel-section", section.title, section_index))
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        for (prop_index, prop) in section.props.iter().enumerate() {
                                            ui.push_id(
                                                (
                                                    "view-panel-property",
                                                    section.title,
                                                    section_index,
                                                    prop.id,
                                                ),
                                                |ui| {
                                                    let height =
                                                        prop.height.unwrap_or(DEFAULT_ROW_HEIGHT);
                                                    let width = ui.available_width();
                                                    let (rect, _) = ui.allocate_exact_size(
                                                        Vec2::new(width, height),
                                                        Sense::hover(),
                                                    );
                                                    if ui
                                                        .add_enabled_ui(editing_enabled, |ui| {
                                                            instance.draw_view_panel_prop(
                                                                section_index,
                                                                prop_index,
                                                                ui,
                                                                rect,
                                                                panel_rect,
                                                            )
                                                        })
                                                        .inner
                                                    {
                                                        changed = true;
                                                    }
                                                },
                                            );
                                        }
                                    });
                            }
                        });
                    });
            },
        );

        if changed {
            self.run_update(node_id);
        }
    }

    fn show_empty_view_panel(&self, ui: &mut Ui, panel_rect: Rect) {
        let painter = ui.painter_at(panel_rect);
        painter.rect_filled(panel_rect, 0.0, Color32::from_rgb(38, 38, 38));
        painter.line_segment(
            [panel_rect.left_top(), panel_rect.left_bottom()],
            Stroke::new(1.0_f32, Color32::from_rgb(70, 70, 70)),
        );
        let content = panel_rect.shrink2(Vec2::new(10.0, 8.0));
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(content)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                ui.set_clip_rect(panel_rect);
                ui.label(RichText::new("View").size(15.0).strong());
                ui.label(RichText::new("No active node").size(11.0).weak());
            },
        );
    }
}
