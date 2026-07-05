//! Blender-style properties panel (N-panel): a resizable strip docked to the
//! right border of the graph view showing the *active* node's low-frequency
//! configuration (design §4.11). Widgets render in screen space at full
//! size, unaffected by graph zoom; edits mutate the same node state as
//! inline controls and run `on_update` through the same path.

use super::NodeGraphWidget;
use crate::model::{NodeId, NodeKind};
use egui::{
    Align, Color32, CursorIcon, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui, UiBuilder, Vec2,
};

const PANEL_MIN_WIDTH: f32 = 220.0;
const PANEL_MAX_WIDTH: f32 = 520.0;
const DEFAULT_ROW_HEIGHT: f32 = 24.0;

pub(super) struct PanelState {
    pub visible: bool,
    pub width: f32,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            visible: true,
            width: 300.0,
        }
    }
}

impl NodeGraphWidget {
    pub(super) fn toggle_panel(&mut self) {
        self.panel.visible = !self.panel.visible;
    }

    /// The node the panel shows: the active (most recently clicked/added)
    /// node while it is still selected, otherwise the newest selected node.
    /// `None` hides the panel.
    fn panel_target(&self) -> Option<NodeId> {
        let shown = |id: &NodeId| {
            self.graph
                .nodes
                .get(id)
                .is_some_and(|node| node.selected && node.kind == NodeKind::Regular)
                && self.runtime.contains_key(id)
        };
        self.active_node.filter(shown).or_else(|| {
            self.graph
                .nodes
                .keys()
                .filter(|id| shown(id))
                .max_by_key(|id| id.0)
                .copied()
        })
    }

    /// Screen rect the panel occupies this frame, `None` while hidden.
    pub(super) fn panel_rect(&self, canvas_rect: Rect) -> Option<Rect> {
        if !self.panel.visible {
            return None;
        }
        self.panel_target()?;
        let width = self
            .panel
            .width
            .clamp(PANEL_MIN_WIDTH, (canvas_rect.width() - 160.0).max(PANEL_MIN_WIDTH));
        Some(Rect::from_min_max(
            Pos2::new(canvas_rect.max.x - width, canvas_rect.min.y),
            canvas_rect.max,
        ))
    }

    /// Allocates the panel's interaction surfaces. Must run before
    /// `handle_input` in the frame so the panel background (registered after
    /// the canvas response) swallows clicks/drags that would otherwise
    /// box-select or deselect through the panel.
    pub(super) fn update_panel_interaction(&mut self, ui: &mut Ui, panel_rect: Rect) {
        let _background = ui.interact(
            panel_rect,
            ui.id().with("props-panel-bg"),
            Sense::click_and_drag(),
        );

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
            self.panel.width = (panel_rect.right() - pointer.x).clamp(PANEL_MIN_WIDTH, PANEL_MAX_WIDTH);
        }
    }

    pub(super) fn show_properties_panel(&mut self, ui: &mut Ui, panel_rect: Rect) {
        let Some(node_id) = self.panel_target() else {
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
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("Name").size(11.0));
                                        ui.text_edit_singleline(&mut node.title);
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("Color").size(11.0));
                                        ui.color_edit_button_srgba(&mut node.header_color);
                                    });
                                });

                            for (section_index, section) in sections.iter().enumerate() {
                                egui::CollapsingHeader::new(section.title)
                                    .id_salt(("props-panel-section", section_index))
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        for (prop_index, height) in
                                            section.prop_heights.iter().enumerate()
                                        {
                                            let height = height.unwrap_or(DEFAULT_ROW_HEIGHT);
                                            let width = ui.available_width();
                                            let (rect, _) = ui.allocate_exact_size(
                                                Vec2::new(width, height),
                                                Sense::hover(),
                                            );
                                            if instance.draw_panel_prop(
                                                section_index,
                                                prop_index,
                                                ui,
                                                rect,
                                                panel_rect,
                                            ) {
                                                changed = true;
                                            }
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
}
