use egui::{Color32, CursorIcon, FontId, Id, Order, Pos2, Rect, Response, Sense, Ui, vec2};

use signal_processing::SimpleTriggerCondition;

use crate::types::{AnalyzerLayout, RowKey};
use crate::viewer::LogicAnalyzerViewer;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleTriggerLane {
    pub channel: usize,
    pub condition: SimpleTriggerCondition,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimpleTriggerEdit {
    pub channel: usize,
    pub condition: SimpleTriggerCondition,
}

pub(crate) struct SimpleTriggerPopup {
    pub(crate) channel: usize,
    pub(crate) position: Pos2,
}

const CONDITIONS: [SimpleTriggerCondition; 6] = [
    SimpleTriggerCondition::Ignore,
    SimpleTriggerCondition::Low,
    SimpleTriggerCondition::High,
    SimpleTriggerCondition::Rising,
    SimpleTriggerCondition::Falling,
    SimpleTriggerCondition::Either,
];

fn condition_name(condition: SimpleTriggerCondition) -> &'static str {
    match condition {
        SimpleTriggerCondition::Ignore => "Ignore",
        SimpleTriggerCondition::Low => "Low",
        SimpleTriggerCondition::High => "High",
        SimpleTriggerCondition::Rising => "Rising edge",
        SimpleTriggerCondition::Falling => "Falling edge",
        SimpleTriggerCondition::Either => "Either edge",
    }
}

fn condition_icon(condition: SimpleTriggerCondition) -> &'static str {
    match condition {
        SimpleTriggerCondition::Ignore => "—",
        SimpleTriggerCondition::Low => "L",
        SimpleTriggerCondition::High => "H",
        SimpleTriggerCondition::Rising => "↑",
        SimpleTriggerCondition::Falling => "↓",
        SimpleTriggerCondition::Either => "↕",
    }
}

impl LogicAnalyzerViewer {
    pub(crate) fn simple_trigger_rect(&self, layout: AnalyzerLayout, row: usize) -> Rect {
        let row_top = self.row_top(layout.labels_rect.top(), row, layout.row_height);
        let height = self
            .row_order
            .get(row)
            .map(|key| self.display_row_height(key, layout.row_height))
            .unwrap_or(layout.row_height);
        Rect::from_center_size(
            Pos2::new(
                layout.labels_rect.left() + 12.0 + layout.trigger_width * 0.5 - 2.0,
                row_top + height * 0.5,
            ),
            vec2(20.0, 20.0),
        )
    }

    pub(crate) fn draw_simple_trigger_icon(
        &self,
        painter: &egui::Painter,
        key: &RowKey,
        rect: Rect,
    ) {
        let RowKey::Channel(channel) = key else {
            return;
        };
        let Some(lane) = self.simple_trigger_lanes.get(channel) else {
            return;
        };
        let color = if lane.enabled {
            Color32::from_rgb(205, 205, 205)
        } else {
            Color32::from_rgb(95, 95, 95)
        };
        painter.rect_stroke(
            rect.shrink(1.5),
            2.0,
            egui::Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            condition_icon(lane.condition),
            FontId::monospace(12.0),
            color,
        );
    }

    pub(crate) fn handle_simple_trigger_input(
        &mut self,
        ui: &Ui,
        _response: &Response,
        layout: AnalyzerLayout,
    ) -> bool {
        let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
            return false;
        };
        let Some(row) = self.row_at_pointer(layout, pointer) else {
            return false;
        };
        let Some(RowKey::Channel(channel)) = self.row_order.get(row) else {
            return false;
        };
        let channel = *channel;
        let Some(lane) = self.simple_trigger_lanes.get(&channel) else {
            return false;
        };
        let rect = self.simple_trigger_rect(layout, row);
        if !rect.contains(pointer) {
            return false;
        }

        self.hovered_input_context = "logic_analyzer.trigger";
        ui.ctx()
            .set_cursor_icon(if self.simple_trigger_editing_enabled && lane.enabled {
                CursorIcon::PointingHand
            } else {
                CursorIcon::NotAllowed
            });
        let tooltip = if lane.enabled {
            format!("Trigger: {}", condition_name(lane.condition))
        } else {
            format!(
                "Trigger input disabled ({})",
                condition_name(lane.condition)
            )
        };
        let icon_response = ui
            .interact(
                rect,
                Id::new(("logic_analyzer_simple_trigger", channel)),
                Sense::click(),
            )
            .on_hover_text(tooltip);
        let open_button = self.input_bindings.pointer_button(
            &["logic_analyzer.trigger", "logic_analyzer"],
            "set_condition",
        );
        if self.simple_trigger_editing_enabled
            && lane.enabled
            && open_button.is_some_and(|button| icon_response.clicked_by(button))
        {
            self.simple_trigger_popup = Some(SimpleTriggerPopup {
                channel,
                position: rect.left_bottom(),
            });
        }
        true
    }

    pub(crate) fn show_simple_trigger_popup(&mut self, ctx: &egui::Context) {
        let Some(popup) = self.simple_trigger_popup.as_ref() else {
            return;
        };
        let channel = popup.channel;
        let position = popup.position;
        let selected = self
            .simple_trigger_lanes
            .get(&channel)
            .map(|lane| lane.condition)
            .unwrap_or_default();
        let mut chosen = None;
        let area = egui::Area::new(Id::new(("logic_analyzer_trigger_popup", channel)))
            .order(Order::Foreground)
            .fixed_pos(position)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(165.0);
                    for condition in CONDITIONS {
                        let label = format!(
                            "{}  {}",
                            condition_icon(condition),
                            condition_name(condition)
                        );
                        if ui.selectable_label(selected == condition, label).clicked() {
                            chosen = Some(condition);
                        }
                    }
                });
            });

        if let Some(condition) = chosen {
            if let Some(lane) = self.simple_trigger_lanes.get_mut(&channel) {
                lane.condition = condition;
            }
            self.pending_simple_trigger_edit = Some(SimpleTriggerEdit { channel, condition });
            self.simple_trigger_popup = None;
            return;
        }

        let close = ctx.input(|input| {
            input.key_pressed(egui::Key::Escape)
                || (input.pointer.any_pressed()
                    && input
                        .pointer
                        .interact_pos()
                        .is_none_or(|pointer| !area.response.rect.contains(pointer)))
        });
        if close {
            self.simple_trigger_popup = None;
        }
    }
}
