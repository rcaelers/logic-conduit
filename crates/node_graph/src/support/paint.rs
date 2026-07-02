use crate::{
    model::{GraphState, SocketId},
    support::ViewState,
};
use egui::epaint::CubicBezierShape;
use egui::{Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use std::collections::HashMap;

pub const SOCKET_RADIUS: f32 = 5.5;

pub fn to_screen_rect(r: Rect, view: &ViewState, origin: Pos2) -> Rect {
    Rect::from_min_max(
        view.canvas_to_screen(origin, r.min),
        view.canvas_to_screen(origin, r.max),
    )
}

pub fn bezier_wire_distance(from: Pos2, to: Pos2, point: Pos2) -> f32 {
    let dx = (to.x - from.x).abs().max(50.0) * 0.5;
    let cp1 = from + Vec2::new(dx, 0.0);
    let cp2 = to - Vec2::new(dx, 0.0);
    (0..=24)
        .map(|k| {
            let t = k as f32 / 24.0;
            let u = 1.0 - t;
            let p = Pos2::new(
                u * u * u * from.x
                    + 3.0 * u * u * t * cp1.x
                    + 3.0 * u * t * t * cp2.x
                    + t * t * t * to.x,
                u * u * u * from.y
                    + 3.0 * u * u * t * cp1.y
                    + 3.0 * u * t * t * cp2.y
                    + t * t * t * to.y,
            );
            point.distance(p)
        })
        .fold(f32::INFINITY, f32::min)
}

pub fn draw_grid(painter: &Painter, rect: Rect, view: &ViewState) {
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_rgb(28, 28, 28));

    let spacing = 20.0_f32;
    let screen_spacing = spacing * view.zoom;
    if screen_spacing < 3.0 {
        return;
    }
    let minor = Color32::from_rgb(38, 38, 38);
    let major = Color32::from_rgb(52, 52, 52);

    let kx0 = ((-view.pan.x) / screen_spacing).floor() as i32 - 1;
    let kx1 = ((rect.width() - view.pan.x) / screen_spacing).ceil() as i32 + 1;
    for k in kx0..=kx1 {
        let sx = rect.min.x + k as f32 * screen_spacing + view.pan.x;
        if sx < rect.min.x || sx > rect.max.x {
            continue;
        }
        let c = if k % 5 == 0 { major } else { minor };
        painter.line_segment(
            [Pos2::new(sx, rect.min.y), Pos2::new(sx, rect.max.y)],
            Stroke::new(1.0_f32, c),
        );
    }

    let ky0 = ((-view.pan.y) / screen_spacing).floor() as i32 - 1;
    let ky1 = ((rect.height() - view.pan.y) / screen_spacing).ceil() as i32 + 1;
    for k in ky0..=ky1 {
        let sy = rect.min.y + k as f32 * screen_spacing + view.pan.y;
        if sy < rect.min.y || sy > rect.max.y {
            continue;
        }
        let c = if k % 5 == 0 { major } else { minor };
        painter.line_segment(
            [Pos2::new(rect.min.x, sy), Pos2::new(rect.max.x, sy)],
            Stroke::new(1.0_f32, c),
        );
    }
}

pub fn draw_frames(
    painter: &Painter,
    graph: &GraphState,
    frame_rects: &HashMap<crate::model::FrameId, Rect>,
    view: &ViewState,
    origin: Pos2,
) {
    for frame in &graph.frames {
        let Some(&bounds) = frame_rects.get(&frame.id) else {
            continue;
        };
        let screen = to_screen_rect(bounds, view, origin);
        let r = CornerRadius::same(6);
        let c = frame.color;
        painter.rect_filled(
            screen,
            r,
            Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), 28),
        );
        painter.rect_stroke(
            screen,
            r,
            Stroke::new(
                1.5_f32,
                Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), 170),
            ),
            egui::StrokeKind::Middle,
        );
        let font_sz = (14.0 * view.zoom).clamp(8.0, 18.0);
        let label_pos = Pos2::new(screen.center().x, screen.min.y + 7.0 * view.zoom);
        let label_font = FontId::proportional(font_sz);
        painter.text(
            label_pos + Vec2::splat(1.0),
            egui::Align2::CENTER_TOP,
            &frame.label,
            label_font.clone(),
            Color32::from_rgba_premultiplied(0, 0, 0, 180),
        );
        painter.text(
            label_pos,
            egui::Align2::CENTER_TOP,
            &frame.label,
            label_font,
            Color32::from_rgba_premultiplied(245, 245, 245, 235),
        );
    }

    for frame in graph.frames.iter().filter(|frame| frame.selected) {
        let Some(&bounds) = frame_rects.get(&frame.id) else {
            continue;
        };
        let screen = to_screen_rect(bounds, view, origin);
        painter.rect_stroke(
            screen,
            CornerRadius::same(6),
            Stroke::new(2.0_f32, Color32::WHITE),
            egui::StrokeKind::Outside,
        );
    }
}

pub fn draw_wire(painter: &Painter, from: Pos2, to: Pos2, color: Color32, width: f32) {
    let dx = (to.x - from.x).abs().max(50.0) * 0.5;
    let cp1 = from + Vec2::new(dx, 0.0);
    let cp2 = to - Vec2::new(dx, 0.0);
    painter.add(CubicBezierShape::from_points_stroke(
        [from, cp1, cp2, to],
        false,
        Color32::TRANSPARENT,
        Stroke::new(width, color),
    ));
}

pub fn draw_connections(
    painter: &Painter,
    graph: &GraphState,
    socket_positions: &HashMap<SocketId, Pos2>,
    wire_width: f32,
) {
    for conn in &graph.connections {
        let Some(&from_p) = socket_positions.get(&conn.from) else {
            continue;
        };
        let Some(&to_p) = socket_positions.get(&conn.to) else {
            continue;
        };
        let color = graph
            .nodes
            .get(&conn.from.node)
            .and_then(|n| n.outputs.get(conn.from.index))
            .map(|s| s.color)
            .unwrap_or(Color32::from_rgb(160, 160, 160));
        draw_wire(painter, from_p, to_p, color, wire_width);
    }
}

pub fn draw_box_select(painter: &Painter, start: Pos2, end: Pos2) {
    let rect = Rect::from_two_pos(start, end);
    painter.rect_filled(
        rect,
        CornerRadius::ZERO,
        Color32::from_rgba_premultiplied(80, 120, 220, 25),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0_f32, Color32::from_rgb(100, 150, 255)),
        egui::StrokeKind::Middle,
    );
}

pub fn draw_knife_line(painter: &Painter, points: &[Pos2]) {
    for w in points.windows(2) {
        painter.line_segment(
            [w[0], w[1]],
            Stroke::new(5.0_f32, Color32::from_rgba_premultiplied(255, 120, 30, 50)),
        );
        painter.line_segment(
            [w[0], w[1]],
            Stroke::new(1.5_f32, Color32::from_rgb(255, 170, 60)),
        );
    }
}

/// Returns true if the cubic bezier wire from `fp` to `tp` crosses the knife segment.
/// All coordinates are in canvas space.
pub fn wire_intersects_knife(fp: Pos2, tp: Pos2, knife_start: Pos2, knife_end: Pos2) -> bool {
    let dx = (tp.x - fp.x).abs().max(50.0) * 0.5;
    let cp1 = fp + Vec2::new(dx, 0.0);
    let cp2 = tp - Vec2::new(dx, 0.0);
    const STEPS: usize = 20;
    let mut prev = fp;
    for i in 1..=STEPS {
        let t = i as f32 / STEPS as f32;
        let next = bezier_point([fp, cp1, cp2, tp], t);
        if segments_intersect(prev, next, knife_start, knife_end) {
            return true;
        }
        prev = next;
    }
    false
}

fn bezier_point(pts: [Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    Pos2::new(
        u * u * u * pts[0].x
            + 3.0 * u * u * t * pts[1].x
            + 3.0 * u * t * t * pts[2].x
            + t * t * t * pts[3].x,
        u * u * u * pts[0].y
            + 3.0 * u * u * t * pts[1].y
            + 3.0 * u * t * t * pts[2].y
            + t * t * t * pts[3].y,
    )
}

fn segments_intersect(p1: Pos2, p2: Pos2, q1: Pos2, q2: Pos2) -> bool {
    let d1 = p2 - p1;
    let d2 = q2 - q1;
    let cross = d1.x * d2.y - d1.y * d2.x;
    if cross.abs() < 1e-8 {
        return false;
    }
    let t = ((q1.x - p1.x) * d2.y - (q1.y - p1.y) * d2.x) / cross;
    let u = ((q1.x - p1.x) * d1.y - (q1.y - p1.y) * d1.x) / cross;
    (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)
}
