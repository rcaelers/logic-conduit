//! The About window: an animated logic-analyzer waveform banner above the
//! usual name/version/copyright block.

const BANNER_HEIGHT: f32 = 132.0;
const WINDOW_WIDTH: f32 = 460.0;
const CORNER_RADIUS: u8 = 10;

/// Scroll speed of the waveform banner, in pixels per second.
const SCROLL_SPEED: f32 = 26.0;
/// Width of one bit cell in the digital traces, in pixels.
const BIT_WIDTH: f32 = 16.0;

pub struct AboutWindow {
    open: bool,
}

impl AboutWindow {
    pub fn new() -> Self {
        Self { open: false }
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.open = false;
            return;
        }

        let frame = egui::Frame::window(&ctx.style_of(ctx.theme()))
            .inner_margin(egui::Margin::ZERO)
            .corner_radius(CORNER_RADIUS);
        let response = egui::Window::new("About")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size([WINDOW_WIDTH, 0.0])
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(frame)
            .show(ctx, |ui| {
                self.banner(ui);
                self.details(ui);
            });

        if let Some(response) = response
            && response.response.clicked_elsewhere()
        {
            self.open = false;
        }

        // The banner scrolls continuously while the window is up.
        if self.open {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    /// Dark hero strip with scrolling digital traces and a decode lane,
    /// echoing the logic analyzer view.
    fn banner(&self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(WINDOW_WIDTH, BANNER_HEIGHT),
            egui::Sense::hover(),
        );
        let painter = ui.painter().with_clip_rect(rect);
        let time = ui.input(|input| input.time) as f32;
        let scroll = time * SCROLL_SPEED;

        painter.rect_filled(
            rect,
            egui::CornerRadius {
                nw: CORNER_RADIUS,
                ne: CORNER_RADIUS,
                sw: 0,
                se: 0,
            },
            egui::Color32::from_rgb(0x12, 0x14, 0x1a),
        );

        // Faint time grid, scrolling with the traces.
        let grid_period = BIT_WIDTH * 4.0;
        let grid_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 9);
        let mut grid_x = rect.left() - (scroll % grid_period);
        while grid_x < rect.right() {
            painter.vline(grid_x, rect.y_range(), egui::Stroke::new(1.0, grid_color));
            grid_x += grid_period;
        }

        let channels: &[(u64, egui::Color32)] = &[
            (0x1157_ec0d_ed11, egui::Color32::from_rgb(0x4a, 0xde, 0x80)),
            (0xca11_ab1e_5eed, egui::Color32::from_rgb(0x38, 0xbd, 0xf8)),
            (0xdec0_ded0_dada, egui::Color32::from_rgb(0xfb, 0xbf, 0x24)),
        ];
        let lanes = channels.len() + 1; // plus the decode lane
        let lane_height = (rect.height() - 24.0) / lanes as f32;
        for (lane, (seed, color)) in channels.iter().enumerate() {
            let center = rect.top() + 16.0 + lane_height * (lane as f32 + 0.5);
            draw_trace(&painter, rect, scroll, *seed, center, lane_height, *color);
        }
        let decode_center = rect.top() + 16.0 + lane_height * (channels.len() as f32 + 0.5);
        draw_decode_lane(&painter, rect, scroll, decode_center);
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let weak = ui.visuals().weak_text_color();
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 24,
                right: 24,
                top: 18,
                bottom: 16,
            })
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("DSL Pipeline Editor")
                            .size(24.0)
                            .strong(),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("Node-graph pipelines for logic analysis")
                            .size(13.0)
                            .color(weak),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Version {} — built with Rust and egui",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .size(12.0)
                        .monospace()
                        .color(weak),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("© 2026 Rob Caelers")
                            .size(12.0)
                            .color(weak),
                    );
                    ui.add_space(12.0);
                    if ui.button("  Close  ").clicked() {
                        self.open = false;
                    }
                });
            });
    }
}

/// Deterministic pseudo-random bit stream: the banner animates but every
/// channel replays the same "capture" forever.
fn stream_bit(seed: u64, index: i64) -> bool {
    let mut x = (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ seed;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x & 1 == 1
}

/// One scrolling square-wave trace with a soft glow under a crisp line.
fn draw_trace(
    painter: &egui::Painter,
    rect: egui::Rect,
    scroll: f32,
    seed: u64,
    center: f32,
    lane_height: f32,
    color: egui::Color32,
) {
    let amplitude = (lane_height * 0.36).min(11.0);
    let level_y = |bit: bool| {
        if bit {
            center - amplitude
        } else {
            center + amplitude
        }
    };

    let first_bit = ((rect.left() + scroll) / BIT_WIDTH).floor() as i64;
    let last_bit = ((rect.right() + scroll) / BIT_WIDTH).ceil() as i64;
    let mut points = Vec::with_capacity(((last_bit - first_bit) * 2 + 2) as usize);
    let mut level = stream_bit(seed, first_bit);
    points.push(egui::pos2(rect.left(), level_y(level)));
    for bit_index in first_bit + 1..=last_bit {
        let next = stream_bit(seed, bit_index);
        let edge_x = bit_index as f32 * BIT_WIDTH - scroll;
        if next != level {
            points.push(egui::pos2(edge_x, level_y(level)));
            points.push(egui::pos2(edge_x, level_y(next)));
            level = next;
        }
    }
    points.push(egui::pos2(rect.right(), level_y(level)));

    let glow = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 26);
    painter.add(egui::Shape::line(
        points.clone(),
        egui::Stroke::new(4.0, glow),
    ));
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, color)));
}

/// Decoder-style lane: rounded chips with hex bytes scrolling past, like a
/// protocol decoder's output row.
fn draw_decode_lane(painter: &egui::Painter, rect: egui::Rect, scroll: f32, center: f32) {
    const CHIP_PERIOD: f32 = 74.0;
    const CHIP_GAP: f32 = 8.0;
    let color = egui::Color32::from_rgb(0xa7, 0x8b, 0xfa);
    let fill = egui::Color32::from_rgba_unmultiplied(0xa7, 0x8b, 0xfa, 24);

    let first = ((rect.left() + scroll) / CHIP_PERIOD).floor() as i64;
    let last = ((rect.right() + scroll) / CHIP_PERIOD).ceil() as i64;
    for index in first..=last {
        let left = index as f32 * CHIP_PERIOD - scroll + CHIP_GAP * 0.5;
        let chip = egui::Rect::from_min_max(
            egui::pos2(left, center - 9.0),
            egui::pos2(left + CHIP_PERIOD - CHIP_GAP, center + 9.0),
        );
        painter.rect(
            chip,
            4.0,
            fill,
            egui::Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        let byte = {
            let mut x = (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xdeca_fbad;
            x ^= x >> 29;
            (x & 0xff) as u8
        };
        painter.text(
            chip.center(),
            egui::Align2::CENTER_CENTER,
            format!("0x{byte:02X}"),
            egui::FontId::monospace(10.0),
            color,
        );
    }
}
