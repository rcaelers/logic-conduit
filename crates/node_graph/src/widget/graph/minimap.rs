use crate::{
    model::{GraphState, NodeId},
    support::ViewState,
};
use egui::{Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use std::collections::HashMap;

const MINIMAP_SCALE: f32 = 0.20;
const MINIMAP_MARGIN_FRACTION: f32 = 0.025;
const MINIMAP_MIN_MARGIN: f32 = 6.0;
const MINIMAP_MAX_MARGIN: f32 = 15.0;

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
    let mini_rect = minimap_rect(canvas_rect);

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

fn minimap_rect(canvas_rect: Rect) -> Rect {
    let margin = (canvas_rect.width().min(canvas_rect.height()) * MINIMAP_MARGIN_FRACTION)
        .clamp(MINIMAP_MIN_MARGIN, MINIMAP_MAX_MARGIN)
        .min(canvas_rect.width() * 0.5)
        .min(canvas_rect.height() * 0.5);
    let available = (canvas_rect.size() - Vec2::splat(margin * 2.0)).max(Vec2::ZERO);
    let preferred = canvas_rect.size() * MINIMAP_SCALE;
    let fit = if preferred.x > 0.0 && preferred.y > 0.0 {
        (available.x / preferred.x)
            .min(available.y / preferred.y)
            .min(1.0)
    } else {
        0.0
    };
    let size = preferred * fit;
    Rect::from_min_size(
        Pos2::new(
            canvas_rect.max.x - margin - size.x,
            canvas_rect.max.y - margin - size.y,
        ),
        size,
    )
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
        let col = Color32::from_rgba_unmultiplied(
            node.header_color.r(),
            node.header_color.g(),
            node.header_color.b(),
            200,
        );
        painter.rect_filled(clamped, CornerRadius::same(1), col);
        if node.selected {
            painter.rect_stroke(
                clamped,
                CornerRadius::same(1),
                Stroke::new(1.0_f32, Color32::WHITE),
                egui::StrokeKind::Outside,
            );
        }
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

#[cfg(test)]
mod tests {
    use super::minimap_rect;
    use egui::{Pos2, Rect};

    #[test]
    fn minimap_scales_with_the_canvas() {
        let small = minimap_rect(Rect::from_min_size(Pos2::ZERO, egui::vec2(1_000.0, 600.0)));
        let large = minimap_rect(Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2(2_000.0, 1_200.0),
        ));

        assert!(large.width() > small.width());
        assert!(large.height() > small.height());
    }

    #[test]
    fn minimap_stays_inside_a_short_canvas() {
        let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(1_000.0, 300.0));
        let minimap = minimap_rect(canvas);

        assert!(canvas.contains_rect(minimap));
    }

    #[test]
    fn minimap_matches_the_canvas_aspect_ratio() {
        let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(1_200.0, 500.0));
        let minimap = minimap_rect(canvas);

        assert!(
            (minimap.width() / minimap.height() - canvas.width() / canvas.height()).abs() < 0.001
        );
    }

    #[test]
    fn minimap_stays_inside_a_tiny_canvas() {
        let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(8.0, 8.0));
        let minimap = minimap_rect(canvas);

        assert!(canvas.contains_rect(minimap));
    }
}
