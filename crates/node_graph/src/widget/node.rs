use crate::{
    model::{
        BadgeSeverity, GraphState, Node, NodeBadge, NodeId, NodeKind, SocketDirection, SocketId,
        SocketShape,
    },
    runtime::{NodeInstance, NodeTypeRegistry},
    support::{ViewState, paint::to_screen_rect},
};
use egui::{Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, Ui, Vec2};

// ── Layout constants ──────────────────────────────────────────────────────────

const NODE_WIDTH: f32 = 200.0;
const NODE_HEADER_HEIGHT: f32 = 22.0;
const SOCKET_ROW_HEIGHT: f32 = 22.0;
const PROP_ROW_HEIGHT: f32 = 22.0;
const SOCKET_RADIUS: f32 = 5.5;
const COLLAPSED_SOCKET_SPACING: f32 = SOCKET_RADIUS * 2.0 + 2.0;
const NODE_PADDING: f32 = 5.0;
const NODE_ROUNDING: f32 = 5.0;
const SOCKET_AREA: f32 = 14.0;
const REROUTE_SIZE: f32 = 24.0;
const COLLAPSE_TOGGLE_SIZE: f32 = 18.0;

// ── Private layout ────────────────────────────────────────────────────────────

struct NodeLayout {
    node_rect: Rect,
    header_rect: Rect,
    collapse_toggle_rect: Rect,
    input_socket_pos: Vec<Option<Pos2>>,
    output_socket_pos: Vec<Option<Pos2>>,
    input_widget_rects: Vec<Option<Rect>>,
    output_widget_rects: Vec<Option<Rect>>,
    prop_rects: Vec<Rect>,
    section_sep_y: Vec<f32>,
}

fn compute_node_layout(node: &Node) -> NodeLayout {
    if node.kind == NodeKind::Reroute {
        let cy = node.pos.y + REROUTE_SIZE / 2.0;
        let node_rect = Rect::from_min_size(node.pos, Vec2::splat(REROUTE_SIZE));
        return NodeLayout {
            node_rect,
            header_rect: node_rect,
            collapse_toggle_rect: Rect::NOTHING,
            input_socket_pos: vec![Some(Pos2::new(node.pos.x, cy))],
            output_socket_pos: vec![Some(Pos2::new(node.pos.x + REROUTE_SIZE, cy))],
            input_widget_rects: vec![],
            output_widget_rects: vec![],
            prop_rects: vec![],
            section_sep_y: vec![],
        };
    }

    if node.collapsed {
        let visible_inputs = node
            .inputs
            .iter()
            .filter(|socket| socket.visible && !socket.hidden)
            .count();
        let visible_outputs = node
            .outputs
            .iter()
            .filter(|socket| socket.visible && !socket.hidden)
            .count();
        let socket_rows = visible_inputs.max(visible_outputs).max(1);
        let height = (NODE_HEADER_HEIGHT * 1.8)
            .max(socket_rows as f32 * COLLAPSED_SOCKET_SPACING + NODE_PADDING * 2.0);
        let node_rect = Rect::from_min_size(node.pos, Vec2::new(NODE_WIDTH, height));
        let header_rect = node_rect;
        let collapse_toggle_rect = Rect::from_center_size(
            Pos2::new(node.pos.x + 18.0, node.pos.y + height * 0.5),
            Vec2::splat(COLLAPSE_TOGGLE_SIZE),
        );

        let mut input_socket_pos = vec![None; node.inputs.len()];
        let mut input_row = 0usize;
        for (index, socket) in node.inputs.iter().enumerate() {
            if !socket.visible || socket.hidden {
                continue;
            }
            input_socket_pos[index] = Some(Pos2::new(
                node.pos.x,
                node.pos.y
                    + NODE_PADDING
                    + input_row as f32 * COLLAPSED_SOCKET_SPACING
                    + COLLAPSED_SOCKET_SPACING * 0.5,
            ));
            input_row += 1;
        }

        let mut output_socket_pos = vec![None; node.outputs.len()];
        let mut output_row = 0usize;
        for (index, socket) in node.outputs.iter().enumerate() {
            if !socket.visible || socket.hidden {
                continue;
            }
            output_socket_pos[index] = Some(Pos2::new(
                node.pos.x + NODE_WIDTH,
                node.pos.y
                    + NODE_PADDING
                    + output_row as f32 * COLLAPSED_SOCKET_SPACING
                    + COLLAPSED_SOCKET_SPACING * 0.5,
            ));
            output_row += 1;
        }

        return NodeLayout {
            node_rect,
            header_rect,
            collapse_toggle_rect,
            input_socket_pos,
            output_socket_pos,
            input_widget_rects: vec![None; node.inputs.len()],
            output_widget_rects: vec![None; node.outputs.len()],
            prop_rects: vec![],
            section_sep_y: vec![],
        };
    }

    let body_top = node.pos.y + NODE_HEADER_HEIGHT + NODE_PADDING;

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
    let collapse_toggle_rect = Rect::from_center_size(
        Pos2::new(
            node.pos.x + NODE_PADDING + COLLAPSE_TOGGLE_SIZE * 0.5,
            node.pos.y + NODE_HEADER_HEIGHT * 0.5,
        ),
        Vec2::splat(COLLAPSE_TOGGLE_SIZE),
    );

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
        collapse_toggle_rect,
        input_socket_pos,
        output_socket_pos,
        input_widget_rects,
        output_widget_rects,
        prop_rects,
        section_sep_y,
    }
}

// ── Node widget ───────────────────────────────────────────────────────────────

pub(crate) struct NodeWidget {
    layout: NodeLayout,
}

impl NodeWidget {
    pub(crate) fn new(node: &Node) -> Self {
        Self {
            layout: compute_node_layout(node),
        }
    }

    // ── Geometry queries ──────────────────────────────────────────────────────

    pub(crate) fn node_rect(&self) -> Rect {
        self.layout.node_rect
    }

    pub(crate) fn header_rect(&self) -> Rect {
        self.layout.header_rect
    }

    pub(crate) fn collapse_toggle_rect(&self) -> Option<Rect> {
        (self.layout.collapse_toggle_rect != Rect::NOTHING)
            .then_some(self.layout.collapse_toggle_rect)
    }

    pub(crate) fn input_socket_pos(&self, i: usize) -> Option<Pos2> {
        self.layout.input_socket_pos.get(i).and_then(|p| *p)
    }

    pub(crate) fn output_socket_pos(&self, i: usize) -> Option<Pos2> {
        self.layout.output_socket_pos.get(i).and_then(|p| *p)
    }

    // ── Drawing ───────────────────────────────────────────────────────────────

    pub(crate) fn draw(
        &self,
        painter: &Painter,
        node: &Node,
        badge: Option<&NodeBadge>,
        status: Option<&str>,
        registry: &NodeTypeRegistry,
        view: &ViewState,
        origin: Pos2,
    ) {
        let l = &self.layout;
        let node_s = to_screen_rect(l.node_rect, view, origin);

        if node.kind == NodeKind::Reroute {
            draw_reroute(painter, node_s, node.selected);
            return;
        }

        if let Some(badge) = badge {
            draw_badge(painter, node_s, badge, view.zoom);
        }

        let s = |p: Pos2| view.canvas_to_screen(origin, p);
        let sz = |v: f32| view.scale(v);
        let header_s = to_screen_rect(l.header_rect, view, origin);
        let r = sz(NODE_ROUNDING).min(255.0) as u8;
        let rounding = CornerRadius::same(r);
        let header_rounding = CornerRadius {
            nw: r,
            ne: r,
            sw: 0,
            se: 0,
        };

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
        if node.collapsed {
            painter.rect_filled(node_s, rounding, node.header_color);
        } else {
            painter.rect_filled(node_s, rounding, body_fill);
            painter.rect_filled(header_s, header_rounding, node.header_color);
        }

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
        draw_collapse_toggle(
            painter,
            to_screen_rect(l.collapse_toggle_rect, view, origin),
            node.collapsed,
        );
        let title_pos = if node.collapsed {
            Pos2::new(header_s.min.x + sz(32.0), header_s.center().y)
        } else {
            header_s.center()
        };
        let title_align = if node.collapsed {
            egui::Align2::LEFT_CENTER
        } else {
            egui::Align2::CENTER_CENTER
        };
        let title = if node.collapsed {
            truncate_title(&node.title)
        } else {
            node.title.clone()
        };
        painter.text(
            title_pos,
            title_align,
            title,
            FontId::proportional(title_sz),
            Color32::WHITE,
        );

        // Live status (progress counter), small at the header's right edge.
        if let Some(status) = status
            && !node.collapsed
            && view.zoom >= 0.5
        {
            painter.text(
                Pos2::new(header_s.right() - sz(6.0), header_s.center().y),
                egui::Align2::RIGHT_CENTER,
                status,
                FontId::proportional((10.0 * view.zoom).clamp(7.0, 12.0)),
                Color32::from_rgba_premultiplied(230, 230, 230, 220),
            );
        }

        if node.collapsed {
            for (i, sock) in node.outputs.iter().enumerate() {
                let Some(pos) = l.output_socket_pos[i] else {
                    continue;
                };
                let (color, shape) = registry.socket_display(sock);
                draw_socket(painter, s(pos), sz(SOCKET_RADIUS), shape, color);
            }

            for (i, sock) in node.inputs.iter().enumerate() {
                let Some(pos) = l.input_socket_pos[i] else {
                    continue;
                };
                let (color, shape) = registry.socket_display(sock);
                draw_socket(painter, s(pos), sz(SOCKET_RADIUS), shape, color);
            }
            return;
        }

        if view.zoom < 0.35 {
            return;
        }

        let sep_color = Color32::from_rgb(62, 62, 62);
        for &sep_y in &l.section_sep_y {
            let p1 = s(Pos2::new(node.pos.x + 4.0, sep_y));
            let p2 = s(Pos2::new(node.pos.x + NODE_WIDTH - 4.0, sep_y));
            painter.line_segment([p1, p2], Stroke::new(1.0_f32, sep_color));
        }

        let lf = FontId::proportional((11.0 * view.zoom).clamp(7.0, 14.0));
        let label_color = Color32::from_rgb(190, 190, 190);

        for (i, sock) in node.outputs.iter().enumerate() {
            let Some(pos) = l.output_socket_pos[i] else {
                continue;
            };
            let sp = s(pos);
            let (color, shape) = registry.socket_display(sock);
            draw_socket(painter, sp, sz(SOCKET_RADIUS), shape, color);
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

        for (i, sock) in node.inputs.iter().enumerate() {
            let Some(pos) = l.input_socket_pos[i] else {
                continue;
            };
            let sp = s(pos);
            let (color, shape) = registry.socket_display(sock);
            draw_socket(painter, sp, sz(SOCKET_RADIUS), shape, color);
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

    // ── Inline controls ───────────────────────────────────────────────────────

    pub(crate) fn show_controls(
        &self,
        ui: &mut Ui,
        node_id: NodeId,
        node: &Node,
        instance: &mut dyn NodeInstance,
        graph: &GraphState,
        view: &ViewState,
        origin: Pos2,
    ) -> bool {
        let l = &self.layout;
        let node_screen_rect = to_screen_rect(l.node_rect, view, origin);
        let zoom = view.zoom;
        let mut any_changed = false;

        for i in 0..node.inputs.len() {
            let sid = SocketId {
                node: node_id,
                index: i,
                direction: SocketDirection::Input,
            };
            if graph.is_input_connected(sid) {
                continue;
            }
            let Some(wr) = l.input_widget_rects.get(i).and_then(|r| *r) else {
                continue;
            };
            let ws = to_screen_rect(wr, view, origin);
            if ws.width() < 30.0 {
                continue;
            }
            // Controls are declared on defs; sockets and defs diverge once
            // variadic groups grow, so map through the socket's def_index.
            let def_index = node.inputs[i].def_index;
            let changed = ui
                .push_id((node_id.0, i), |ui| {
                    instance.draw_input_control(def_index, ui, ws, zoom, node_screen_rect)
                })
                .inner;
            if changed {
                any_changed = true;
            }
        }

        for i in 0..node.outputs.len() {
            let Some(wr) = l.output_widget_rects.get(i).and_then(|r| *r) else {
                continue;
            };
            let ws = to_screen_rect(wr, view, origin);
            if ws.width() < 30.0 {
                continue;
            }
            let def_index = node.outputs[i].def_index;
            let changed = ui
                .push_id(("output", node_id.0, i), |ui| {
                    instance.draw_output_control(def_index, ui, ws, zoom, node_screen_rect)
                })
                .inner;
            if changed {
                any_changed = true;
            }
        }

        for pi in 0..node.property_count {
            let Some(pr) = l.prop_rects.get(pi).copied() else {
                continue;
            };
            let ws = to_screen_rect(pr, view, origin);
            if ws.width() < 40.0 {
                continue;
            }
            let changed = ui
                .push_id((node_id.0, pi), |ui| {
                    instance.draw_property(pi, ui, ws, zoom, node_screen_rect)
                })
                .inner;
            if changed {
                any_changed = true;
            }
        }

        any_changed
    }
}

// ── Private draw helpers ──────────────────────────────────────────────────────

/// Status pill under the node body. Wraps at ~1.6 node widths so long
/// compile errors stay readable without dwarfing the node.
fn draw_badge(painter: &Painter, node_screen_rect: Rect, badge: &NodeBadge, zoom: f32) {
    if zoom < 0.35 {
        return;
    }
    let (fill, icon) = match badge.severity {
        BadgeSeverity::Info => (Color32::from_rgb(62, 62, 62), "ℹ"),
        BadgeSeverity::Warning => (Color32::from_rgb(140, 105, 30), "⚠"),
        BadgeSeverity::Error => (Color32::from_rgb(150, 45, 45), "⚠"),
    };
    let font = FontId::proportional((11.0 * zoom).clamp(8.0, 14.0));
    let wrap_width = node_screen_rect.width() * 1.6;
    let galley = painter.layout(
        format!("{icon} {}", badge.text),
        font,
        Color32::from_rgb(235, 235, 235),
        wrap_width,
    );
    let pad = Vec2::new(6.0, 3.0);
    let pos = node_screen_rect.left_bottom() + Vec2::new(0.0, 6.0 * zoom.max(0.5));
    let rect = Rect::from_min_size(pos, galley.size() + pad * 2.0);
    painter.rect_filled(rect, CornerRadius::same(4), fill);
    painter.galley(pos + pad, galley, Color32::WHITE);
}

fn draw_collapse_toggle(painter: &Painter, rect: Rect, collapsed: bool) {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.28;
    let pts = if collapsed {
        vec![
            Pos2::new(center.x - radius * 0.45, center.y - radius),
            Pos2::new(center.x - radius * 0.45, center.y + radius),
            Pos2::new(center.x + radius * 0.75, center.y),
        ]
    } else {
        vec![
            Pos2::new(center.x - radius, center.y - radius * 0.45),
            Pos2::new(center.x + radius, center.y - radius * 0.45),
            Pos2::new(center.x, center.y + radius * 0.75),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        pts,
        Color32::WHITE,
        Stroke::NONE,
    ));
}

fn truncate_title(title: &str) -> String {
    const MAX_CHARS: usize = 24;
    if title.chars().count() <= MAX_CHARS {
        return title.to_owned();
    }
    let mut truncated: String = title.chars().take(MAX_CHARS - 1).collect();
    truncated.push('…');
    truncated
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
    let outline = socket_outline_color(color);
    let outline_stroke = Stroke::new(2.0_f32, outline);
    match shape {
        SocketShape::Circle => {
            painter.circle_filled(pos, radius, color);
            painter.circle_stroke(pos, radius, outline_stroke);
        }
        SocketShape::Diamond => {
            // A diamond (rotated square) with vertices at `radius` has area 2*r^2,
            // versus a circle's pi*r^2 — noticeably smaller at equal radius.
            // Scale up so it reads as the same visual size as the circle.
            let r = radius * 1.25;
            let pts = vec![
                Pos2::new(pos.x, pos.y - r),
                Pos2::new(pos.x + r, pos.y),
                Pos2::new(pos.x, pos.y + r),
                Pos2::new(pos.x - r, pos.y),
            ];
            painter.add(egui::Shape::convex_polygon(pts, color, outline_stroke));
        }
        SocketShape::Square => {
            let rect = Rect::from_center_size(pos, Vec2::splat(radius * 1.7));
            painter.rect_filled(rect, CornerRadius::ZERO, color);
            painter.rect_stroke(
                rect,
                CornerRadius::ZERO,
                outline_stroke,
                egui::StrokeKind::Outside,
            );
        }
        SocketShape::Triangle => {
            let pts = vec![
                Pos2::new(pos.x, pos.y - radius),
                Pos2::new(pos.x + radius, pos.y + radius),
                Pos2::new(pos.x - radius, pos.y + radius),
            ];
            painter.add(egui::Shape::convex_polygon(pts, color, outline_stroke));
        }
    }
}

fn socket_outline_color(color: Color32) -> Color32 {
    let luminance =
        0.2126 * color.r() as f32 + 0.7152 * color.g() as f32 + 0.0722 * color.b() as f32;
    if luminance < 95.0 {
        Color32::from_rgb(210, 210, 210)
    } else {
        Color32::from_rgb(20, 20, 20)
    }
}
