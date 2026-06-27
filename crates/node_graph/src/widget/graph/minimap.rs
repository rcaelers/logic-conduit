use crate::{
    model::{GraphState, NodeId},
    support::ViewState,
};
use egui::{Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use std::collections::HashMap;

pub struct MinimapInfo {
    pub mini_rect: Rect,
    canvas_bounds: Rect,
    offset: Vec2,
    scale: f32,
}

impl MinimapInfo {
    fn new(canvas_bounds: Rect, mini_rect: Rect) -> Self {
        let scale_x = mini_rect.width() / canvas_bounds.width();
        let scale_y = mini_rect.height() / canvas_bounds.height();
        let scale = scale_x.min(scale_y);
        let scaled = canvas_bounds.size() * scale;
        let offset = (mini_rect.min + (mini_rect.size() - scaled) / 2.0).to_vec2();
        Self {
            mini_rect,
            canvas_bounds,
            offset,
            scale,
        }
    }

    pub fn canvas_to_mini(&self, p: Pos2) -> Pos2 {
        Pos2::new(
            self.offset.x + (p.x - self.canvas_bounds.min.x) * self.scale,
            self.offset.y + (p.y - self.canvas_bounds.min.y) * self.scale,
        )
    }

    pub fn mini_to_canvas(&self, p: Pos2) -> Pos2 {
        Pos2::new(
            (p.x - self.offset.x) / self.scale + self.canvas_bounds.min.x,
            (p.y - self.offset.y) / self.scale + self.canvas_bounds.min.y,
        )
    }
}

pub fn compute_minimap(
    node_rects: impl Iterator<Item = Rect>,
    canvas_rect: Rect,
) -> (MinimapInfo, Rect) {
    let mini_rect = Rect::from_min_max(
        Pos2::new(canvas_rect.max.x - 215.0, canvas_rect.max.y - 145.0),
        Pos2::new(canvas_rect.max.x - 15.0, canvas_rect.max.y - 15.0),
    );

    let mut bounds: Option<Rect> = None;
    for nr in node_rects {
        bounds = Some(bounds.map_or(nr, |b| b.union(nr)));
    }
    let canvas_bounds = bounds
        .unwrap_or_else(|| Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 600.0)))
        .expand(100.0);

    let info = MinimapInfo::new(canvas_bounds, mini_rect);
    (info, mini_rect)
}

pub fn draw_minimap(
    painter: &Painter,
    info: &MinimapInfo,
    graph: &GraphState,
    node_rects: &HashMap<NodeId, Rect>,
    view: &ViewState,
    canvas_rect: Rect,
) {
    let mini_rect = info.mini_rect;

    painter.rect_filled(
        mini_rect,
        CornerRadius::same(4),
        Color32::from_rgba_unmultiplied(16, 16, 16, 215),
    );
    painter.rect_stroke(
        mini_rect,
        CornerRadius::same(4),
        Stroke::new(1.0_f32, Color32::from_rgb(65, 65, 65)),
        egui::StrokeKind::Middle,
    );

    for (&id, &nr) in node_rects {
        let node = &graph.nodes[&id];
        let mn = info.canvas_to_mini(nr.min);
        let mx = info.canvas_to_mini(nr.max);
        let clamped = Rect::from_min_max(mn, mx).intersect(mini_rect);
        if clamped.area() < 0.3 {
            continue;
        }
        let col = if node.selected {
            Color32::from_rgba_unmultiplied(180, 180, 255, 220)
        } else {
            Color32::from_rgba_unmultiplied(
                node.header_color.r(),
                node.header_color.g(),
                node.header_color.b(),
                200,
            )
        };
        painter.rect_filled(clamped, CornerRadius::same(1), col);
    }

    let origin = canvas_rect.min;
    let vp_tl = view.screen_to_canvas(origin, canvas_rect.min);
    let vp_br = view.screen_to_canvas(origin, canvas_rect.max);
    let vp_rect = Rect::from_min_max(info.canvas_to_mini(vp_tl), info.canvas_to_mini(vp_br));
    let vp_clipped = vp_rect.intersect(mini_rect);
    if vp_clipped.area() > 0.0 {
        painter.rect_filled(
            vp_clipped,
            CornerRadius::same(1),
            Color32::from_rgba_unmultiplied(255, 255, 255, 8),
        );
    }
    painter.rect_stroke(
        vp_rect,
        CornerRadius::same(1),
        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(200, 200, 200, 180)),
        egui::StrokeKind::Middle,
    );

    painter.text(
        Pos2::new(mini_rect.min.x + 5.0, mini_rect.min.y + 3.0),
        egui::Align2::LEFT_TOP,
        "Overview  [M]",
        FontId::proportional(9.0),
        Color32::from_rgba_unmultiplied(140, 140, 140, 180),
    );
}
