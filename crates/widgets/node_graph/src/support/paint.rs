use std::collections::HashMap;

use egui::epaint::CubicBezierShape;
use egui::{Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, Vec2};

use super::ViewState;
use crate::model::{Connection, GraphState, SocketId};

pub(crate) const SOCKET_RADIUS: f32 = 5.5;

pub(crate) fn to_screen_rect(r: Rect, view: &ViewState, origin: Pos2) -> Rect {
    Rect::from_min_max(
        view.canvas_to_screen(origin, r.min),
        view.canvas_to_screen(origin, r.max),
    )
}

/// Points sampled along the same cubic bezier that `draw_wire` renders.
fn bezier_wire_points(from: Pos2, to: Pos2, steps: usize) -> impl Iterator<Item = Pos2> {
    let dx = (to.x - from.x).abs().max(50.0) * 0.5;
    let cp1 = from + Vec2::new(dx, 0.0);
    let cp2 = to - Vec2::new(dx, 0.0);
    (0..=steps).map(move |k| {
        let t = k as f32 / steps as f32;
        let u = 1.0 - t;
        Pos2::new(
            u * u * u * from.x
                + 3.0 * u * u * t * cp1.x
                + 3.0 * u * t * t * cp2.x
                + t * t * t * to.x,
            u * u * u * from.y
                + 3.0 * u * u * t * cp1.y
                + 3.0 * u * t * t * cp2.y
                + t * t * t * to.y,
        )
    })
}

pub(crate) fn bezier_wire_distance(from: Pos2, to: Pos2, point: Pos2) -> f32 {
    bezier_wire_points(from, to, 24)
        .map(|p| point.distance(p))
        .fold(f32::INFINITY, f32::min)
}

/// Whether the wire passes through `rect`. Sampled densely enough that even
/// a collapsed node can't fit between consecutive samples of a long wire.
pub(crate) fn bezier_wire_intersects_rect(from: Pos2, to: Pos2, rect: Rect) -> bool {
    bezier_wire_points(from, to, 64).any(|p| rect.contains(p))
}

pub(crate) fn draw_grid(painter: &Painter, rect: Rect, view: &ViewState) {
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

pub(crate) fn draw_frames(
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

pub(crate) fn draw_wire(painter: &Painter, from: Pos2, to: Pos2, color: Color32, width: f32) {
    let dx = (to.x - from.x).abs().max(50.0) * 0.5;
    let cp1 = from + Vec2::new(dx, 0.0);
    let cp2 = to - Vec2::new(dx, 0.0);
    let points = [from, cp1, cp2, to];
    painter.add(CubicBezierShape::from_points_stroke(
        points,
        false,
        Color32::TRANSPARENT,
        Stroke::new(width + 2.0, Color32::from_rgba_premultiplied(0, 0, 0, 170)),
    ));
    painter.add(CubicBezierShape::from_points_stroke(
        points,
        false,
        Color32::TRANSPARENT,
        Stroke::new(width, color),
    ));
}

/// Same curve as `draw_wire`, but dashed — for the internal pass-through
/// link a muted node draws between one of its own input and output sockets
/// (Blender's mute convention: external wires stay solid, an internal
/// dashed link shows what the node passes straight through).
pub(crate) fn draw_wire_dashed(
    painter: &Painter,
    from: Pos2,
    to: Pos2,
    color: Color32,
    width: f32,
) {
    let points: Vec<Pos2> = bezier_wire_points(from, to, 48).collect();
    painter.extend(egui::Shape::dashed_line(
        &points,
        Stroke::new(width + 2.0, Color32::from_rgba_premultiplied(0, 0, 0, 170)),
        6.0,
        4.0,
    ));
    painter.extend(egui::Shape::dashed_line(
        &points,
        Stroke::new(width, color),
        6.0,
        4.0,
    ));
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WireEmphasis {
    Normal,
    /// Connected to a selected node, or a valid insert target for the
    /// dragged node: brighter and thicker.
    Highlight,
    /// Insert target the dragged node cannot splice into: dimmed.
    Muted,
}

fn brighten_wire_color(base: Color32) -> Color32 {
    let mix = |channel: u8| ((channel as f32 * 0.48) + (255.0 * 0.52)).round() as u8;
    Color32::from_rgba_unmultiplied(mix(base.r()), mix(base.g()), mix(base.b()), 255)
}

fn mute_wire_color(base: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        (base.r() as f32 * 0.35) as u8,
        (base.g() as f32 * 0.35) as u8,
        (base.b() as f32 * 0.35) as u8,
        255,
    )
}

pub(crate) fn draw_connections(
    painter: &Painter,
    graph: &GraphState,
    registry: &crate::runtime::NodeTypeRegistry,
    socket_positions: &HashMap<SocketId, Pos2>,
    wire_width: f32,
    emphasis: impl Fn(usize, &Connection) -> WireEmphasis,
) {
    let mut highlighted = Vec::new();
    for (idx, conn) in graph.connections.iter().enumerate() {
        let emphasis = emphasis(idx, conn);
        if emphasis == WireEmphasis::Highlight {
            highlighted.push((idx, conn));
            continue;
        }
        draw_connection(
            painter,
            graph,
            registry,
            socket_positions,
            wire_width,
            conn,
            emphasis,
        );
    }
    for (idx, conn) in highlighted {
        draw_connection(
            painter,
            graph,
            registry,
            socket_positions,
            wire_width,
            conn,
            emphasis(idx, conn),
        );
    }
}

fn draw_connection(
    painter: &Painter,
    graph: &GraphState,
    registry: &crate::runtime::NodeTypeRegistry,
    socket_positions: &HashMap<SocketId, Pos2>,
    wire_width: f32,
    conn: &Connection,
    emphasis: WireEmphasis,
) {
    let Some(&from_p) = socket_positions.get(&conn.from) else {
        return;
    };
    let Some(&to_p) = socket_positions.get(&conn.to) else {
        return;
    };
    // `socket.color` is the socket's *idle* look; a resolved polymorphic
    // socket (e.g. a reroute's `Any` output taking on whatever flows
    // through it) needs the connected type's registry-wide color instead —
    // the same lookup socket dots already use — or the wire renders in the
    // socket's flat default color forever, mismatched with the dot beside it.
    let base = graph
        .nodes
        .get(&conn.from.node)
        .and_then(|n| n.outputs.get(conn.from.index))
        .map(|s| registry.socket_display(s).0)
        .unwrap_or(Color32::from_rgb(160, 160, 160));
    let (color, width) = match emphasis {
        WireEmphasis::Normal => (base, wire_width),
        WireEmphasis::Highlight => (brighten_wire_color(base), wire_width * 2.0),
        WireEmphasis::Muted => (mute_wire_color(base), wire_width),
    };
    draw_wire(painter, from_p, to_p, color, width);
}

pub(crate) fn draw_box_select(painter: &Painter, start: Pos2, end: Pos2) {
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

pub(crate) fn draw_knife_line(painter: &Painter, points: &[Pos2]) {
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
pub(crate) fn wire_intersects_knife(
    fp: Pos2,
    tp: Pos2,
    knife_start: Pos2,
    knife_end: Pos2,
) -> bool {
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
