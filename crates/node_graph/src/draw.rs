use crate::{
    graph::{GraphState, Node, NodeId, NodeKind, SocketId},
    types::SocketShape,
    view::ViewState,
};
use egui::epaint::CubicBezierShape;
use egui::{Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use std::collections::HashMap;

pub const NODE_WIDTH: f32 = 200.0;
pub const NODE_HEADER_HEIGHT: f32 = 22.0;
pub const SOCKET_ROW_HEIGHT: f32 = 22.0;
pub const PROP_ROW_HEIGHT: f32 = 22.0;
pub const SOCKET_RADIUS: f32 = 5.5;
pub const NODE_PADDING: f32 = 5.0;
pub const NODE_ROUNDING: f32 = 5.0;
pub const SOCKET_AREA: f32 = 14.0;
const REROUTE_SIZE: f32 = 24.0;

pub struct NodeLayout {
    pub node_rect: Rect,
    pub header_rect: Rect,
    pub input_socket_pos: Vec<Option<Pos2>>, // None for hidden sockets
    pub output_socket_pos: Vec<Option<Pos2>>,
    pub input_widget_rects: Vec<Option<Rect>>,
    pub output_widget_rects: Vec<Option<Rect>>,
    pub prop_rects: Vec<Rect>,
    pub section_sep_y: Vec<f32>, // canvas Y of dividers between output/prop/input sections
}

pub fn compute_node_layout(node: &Node) -> NodeLayout {
    if node.kind == NodeKind::Reroute {
        let cy = node.pos.y + REROUTE_SIZE / 2.0;
        let node_rect = Rect::from_min_size(node.pos, Vec2::splat(REROUTE_SIZE));
        return NodeLayout {
            node_rect,
            header_rect: node_rect,
            input_socket_pos: vec![Some(Pos2::new(node.pos.x, cy))],
            output_socket_pos: vec![Some(Pos2::new(node.pos.x + REROUTE_SIZE, cy))],
            input_widget_rects: vec![],
            output_widget_rects: vec![],
            prop_rects: vec![],
            section_sep_y: vec![],
        };
    }

    let body_top = node.pos.y + NODE_HEADER_HEIGHT + NODE_PADDING;

    // == Output sockets — top section, right side ==
    let mut output_socket_pos = vec![None; node.outputs.len()];
    let mut output_widget_rects = vec![None; node.outputs.len()];
    let mut vis_row = 0usize;
    for (i, s) in node.outputs.iter().enumerate() {
        if !s.visible || s.hidden {
            continue;
        }
        let row_y = body_top + vis_row as f32 * SOCKET_ROW_HEIGHT;
        output_socket_pos[i] = Some(Pos2::new(
            node.pos.x + NODE_WIDTH,
            row_y + SOCKET_ROW_HEIGHT * 0.5,
        ));
        if s.has_control {
            output_widget_rects[i] = Some(Rect::from_min_size(
                Pos2::new(node.pos.x + 4.0, row_y + 2.0),
                Vec2::new(NODE_WIDTH - SOCKET_AREA - 4.0, SOCKET_ROW_HEIGHT - 4.0),
            ));
        }
        vis_row += 1;
    }
    let vis_outputs = vis_row;
    let output_h = vis_outputs as f32 * SOCKET_ROW_HEIGHT;

    // == Properties — middle section ==
    let prop_count = node.property_count;
    let prop_start_y = body_top + output_h;
    let prop_h = prop_count as f32 * PROP_ROW_HEIGHT;
    let prop_rects: Vec<Rect> = (0..prop_count)
        .map(|i| {
            let y = prop_start_y + i as f32 * PROP_ROW_HEIGHT;
            Rect::from_min_size(
                Pos2::new(node.pos.x + 4.0, y + 1.0),
                Vec2::new(NODE_WIDTH - 8.0, PROP_ROW_HEIGHT - 2.0),
            )
        })
        .collect();

    // == Input sockets — bottom section, left side ==
    let input_start_y = prop_start_y + prop_h;
    let mut input_socket_pos = vec![None; node.inputs.len()];
    let mut input_widget_rects = vec![None; node.inputs.len()];
    vis_row = 0;
    for (i, s) in node.inputs.iter().enumerate() {
        if !s.visible || s.hidden {
            continue;
        }
        let row_y = input_start_y + vis_row as f32 * SOCKET_ROW_HEIGHT;
        input_socket_pos[i] = Some(Pos2::new(node.pos.x, row_y + SOCKET_ROW_HEIGHT * 0.5));
        // Control rect spans from after the socket circle to the right edge.
        let has_control = node.inputs.get(i).is_some_and(|s| s.has_control);
        if has_control {
            input_widget_rects[i] = Some(Rect::from_min_size(
                Pos2::new(node.pos.x + SOCKET_AREA, row_y + 2.0),
                Vec2::new(NODE_WIDTH - SOCKET_AREA - 4.0, SOCKET_ROW_HEIGHT - 4.0),
            ));
        }
        vis_row += 1;
    }
    let vis_inputs = vis_row;
    let input_h = vis_inputs as f32 * SOCKET_ROW_HEIGHT;

    let body_h = NODE_PADDING + output_h + prop_h + input_h + NODE_PADDING;
    let node_rect =
        Rect::from_min_size(node.pos, Vec2::new(NODE_WIDTH, NODE_HEADER_HEIGHT + body_h));
    let header_rect = Rect::from_min_size(node.pos, Vec2::new(NODE_WIDTH, NODE_HEADER_HEIGHT));

    let mut section_sep_y = Vec::new();
    if vis_outputs > 0 && (prop_count > 0 || vis_inputs > 0) {
        section_sep_y.push(body_top + output_h);
    }
    if prop_count > 0 && vis_inputs > 0 {
        section_sep_y.push(input_start_y);
    }

    NodeLayout {
        node_rect,
        header_rect,
        input_socket_pos,
        output_socket_pos,
        input_widget_rects,
        output_widget_rects,
        prop_rects,
        section_sep_y,
    }
}

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
    layouts: &HashMap<NodeId, NodeLayout>,
    view: &ViewState,
    origin: Pos2,
) {
    for frame in &graph.frames {
        let mut bounds: Option<Rect> = None;
        for &id in &frame.node_ids {
            if let Some(l) = layouts.get(&id) {
                bounds = Some(bounds.map_or(l.node_rect, |b| b.union(l.node_rect)));
            }
        }
        let Some(bounds) = bounds else { continue };
        let padded = bounds.expand(20.0);
        let screen = to_screen_rect(padded, view, origin);
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
                Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), 140),
            ),
            egui::StrokeKind::Middle,
        );
        let font_sz = (13.0 * view.zoom).clamp(8.0, 16.0);
        painter.text(
            Pos2::new(screen.min.x + 7.0, screen.min.y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            &frame.label,
            FontId::proportional(font_sz),
            Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), 200),
        );
    }
}

pub fn draw_node(
    painter: &Painter,
    node: &Node,
    layout: &NodeLayout,
    view: &ViewState,
    origin: Pos2,
) {
    let node_s = to_screen_rect(layout.node_rect, view, origin);

    if node.kind == NodeKind::Reroute {
        draw_reroute(painter, node_s, node.selected);
        return;
    }

    let s = |p: Pos2| view.canvas_to_screen(origin, p);
    let sz = |v: f32| view.scale(v);
    let header_s = to_screen_rect(layout.header_rect, view, origin);
    let r = sz(NODE_ROUNDING).min(255.0) as u8;
    let rounding = CornerRadius::same(r);
    let header_rounding = CornerRadius {
        nw: r,
        ne: r,
        sw: 0,
        se: 0,
    };

    // Drop shadow
    painter.rect_filled(
        node_s.translate(Vec2::new(3.0, 3.0)),
        rounding,
        Color32::from_black_alpha(60),
    );

    let body_fill = if node.selected {
        Color32::from_rgb(68, 68, 68)
    } else {
        Color32::from_rgb(48, 48, 48)
    };
    painter.rect_filled(node_s, rounding, body_fill);

    painter.rect_filled(header_s, header_rounding, node.header_color);

    let (bw, bc) = if node.selected {
        (2.0_f32, Color32::WHITE)
    } else {
        (1.5_f32, Color32::from_rgb(90, 90, 90))
    };
    painter.rect_stroke(
        node_s,
        rounding,
        Stroke::new(bw, bc),
        egui::StrokeKind::Outside,
    );
    let title_sz = (14.0 * view.zoom).clamp(8.0, 18.0);
    painter.text(
        header_s.center(),
        egui::Align2::CENTER_CENTER,
        &node.title,
        FontId::proportional(title_sz),
        Color32::WHITE,
    );

    if view.zoom < 0.35 {
        return;
    }

    let sep_color = Color32::from_rgb(62, 62, 62);
    for &sep_y in &layout.section_sep_y {
        let p1 = s(Pos2::new(node.pos.x + 4.0, sep_y));
        let p2 = s(Pos2::new(node.pos.x + NODE_WIDTH - 4.0, sep_y));
        painter.line_segment([p1, p2], Stroke::new(1.0_f32, sep_color));
    }

    let lf = FontId::proportional((11.0 * view.zoom).clamp(7.0, 14.0));
    let label_color = Color32::from_rgb(190, 190, 190);

    // Output sockets — right side; labels are replaced by inline controls when present.
    for (i, sock) in node.outputs.iter().enumerate() {
        let Some(pos) = layout.output_socket_pos[i] else {
            continue;
        };
        let sp = s(pos);
        draw_socket(painter, sp, sz(SOCKET_RADIUS), sock.shape, sock.color);
        if !sock.has_control {
            painter.text(
                Pos2::new(sp.x - sz(SOCKET_RADIUS + 4.0), sp.y),
                egui::Align2::RIGHT_CENTER,
                &sock.name,
                lf.clone(),
                label_color,
            );
        }
    }

    // Input sockets — left side; label shown only when there is no inline control.
    for (i, sock) in node.inputs.iter().enumerate() {
        let Some(pos) = layout.input_socket_pos[i] else {
            continue;
        };
        let sp = s(pos);
        draw_socket(painter, sp, sz(SOCKET_RADIUS), sock.shape, sock.color);
        let has_control = node.inputs.get(i).is_some_and(|s| s.has_control);
        if !has_control {
            painter.text(
                Pos2::new(sp.x + sz(SOCKET_RADIUS + 4.0), sp.y),
                egui::Align2::LEFT_CENTER,
                &sock.name,
                lf.clone(),
                label_color,
            );
        }
    }
}

fn draw_reroute(painter: &Painter, screen_rect: Rect, selected: bool) {
    let c = screen_rect.center();
    let r = screen_rect.width() / 2.0;
    let fill = if selected {
        Color32::from_rgb(80, 80, 80)
    } else {
        Color32::from_rgb(55, 55, 55)
    };
    let (sw, sc) = if selected {
        (2.0_f32, Color32::WHITE)
    } else {
        (1.5_f32, Color32::from_rgb(140, 140, 140))
    };
    let pts = vec![
        Pos2::new(c.x, c.y - r),
        Pos2::new(c.x + r, c.y),
        Pos2::new(c.x, c.y + r),
        Pos2::new(c.x - r, c.y),
    ];
    painter.add(egui::Shape::convex_polygon(pts, fill, Stroke::new(sw, sc)));
    painter.circle_filled(c, r * 0.35, sc);
}

fn draw_socket(painter: &Painter, pos: Pos2, radius: f32, shape: SocketShape, color: Color32) {
    match shape {
        SocketShape::Circle => {
            painter.circle_filled(pos, radius, Color32::from_rgb(28, 28, 28));
            painter.circle_stroke(pos, radius - 1.5, Stroke::new(2.0_f32, color));
        }
        SocketShape::Diamond => {
            let pts = vec![
                Pos2::new(pos.x, pos.y - radius),
                Pos2::new(pos.x + radius, pos.y),
                Pos2::new(pos.x, pos.y + radius),
                Pos2::new(pos.x - radius, pos.y),
            ];
            painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        SocketShape::Square => {
            painter.rect_filled(
                Rect::from_center_size(pos, Vec2::splat(radius * 1.7)),
                CornerRadius::ZERO,
                color,
            );
        }
        SocketShape::Triangle => {
            let pts = vec![
                Pos2::new(pos.x, pos.y - radius),
                Pos2::new(pos.x + radius, pos.y + radius),
                Pos2::new(pos.x - radius, pos.y + radius),
            ];
            painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
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
        painter.line_segment([w[0], w[1]], Stroke::new(5.0_f32, Color32::from_rgba_premultiplied(255, 120, 30, 50)));
        painter.line_segment([w[0], w[1]], Stroke::new(1.5_f32, Color32::from_rgb(255, 170, 60)));
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
        u*u*u*pts[0].x + 3.0*u*u*t*pts[1].x + 3.0*u*t*t*pts[2].x + t*t*t*pts[3].x,
        u*u*u*pts[0].y + 3.0*u*u*t*pts[1].y + 3.0*u*t*t*pts[2].y + t*t*t*pts[3].y,
    )
}

fn segments_intersect(p1: Pos2, p2: Pos2, q1: Pos2, q2: Pos2) -> bool {
    let d1 = p2 - p1;
    let d2 = q2 - q1;
    let cross = d1.x * d2.y - d1.y * d2.x;
    if cross.abs() < 1e-8 { return false; }
    let t = ((q1.x - p1.x) * d2.y - (q1.y - p1.y) * d2.x) / cross;
    let u = ((q1.x - p1.x) * d1.y - (q1.y - p1.y) * d1.x) / cross;
    (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)
}
