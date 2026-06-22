//! CCD Capture Viewer
//!
//! Displays raw parallel bus capture data as a 2D image for investigating
//! how a CCD scanner transmits pixel data.
//!
//! Pixel format: 6 bytes per pixel — G, B, R (hi+lo each): [G-hi, G-lo, B-hi, B-lo, R-hi, R-lo].
//!
//! Architecture:
//!   Phase 1 (decode): raw bytes → cached pixel image (`Vec<u32>`)
//!     Only re-run when width, offset, mode, or deltas change.
//!   Phase 2 (blit): cached image → window framebuffer
//!     Run on every redraw (pan, zoom). Box-averages when zoomed out.
//!
//! Controls:
//!   Arrow keys:        Pan (Shift = fine, Page Up/Down = fast vertical)
//!   Home/End:          Jump to start/end of data
//!   G:                 Toggle display mode: Grayscale ↔ Color
//!   D/Shift+D:         Adjust color delta +1 / -1 (both B–G and G–R together)
//!   B/Shift+B:         Adjust B–G line delta +1 / -1
//!   R/Shift+R:         Adjust G–R line delta +1 / -1
//!   L:                 Lock B–G and G–R deltas (set both equal)
//!   A:                 Auto-detect color channel deltas (cross-correlation)
//!   +/=:               Zoom in (×1.25)
//!   -:                 Zoom out (÷1.25)
//!   0:                 Reset zoom to 1.0 (fit to width)
//!   P:                 1:1 pixel mapping (1 source pixel = 1 screen pixel)
//!   T:                 10:1 pixel mapping (1 source pixel = 10×10 screen pixels)
//!   1-9:               Set zoom level (1x-9x)
//!   W:                 Print current width to console
//!   Escape:            Quit
//!
//! Usage:
//!   cargo run --release --example ccd_viewer -- \
//!       --file _captures/red/capture_0020.bin \
//!       --width 10600

use clap::Parser;
use memmap2::Mmap;
use minifb::{Key, KeyRepeat, Window, WindowOptions};
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(author, version, about = "CCD capture data viewer")]
struct Args {
    /// Path to binary capture file
    #[arg(short, long)]
    file: PathBuf,

    /// Initial line width in bytes (auto-detected from TGCK if omitted)
    #[arg(short, long)]
    width: Option<usize>,

    /// Window width in pixels
    #[arg(long, default_value_t = 3600)]
    win_width: usize,

    /// Window height in pixels
    #[arg(long, default_value_t = 1500)]
    win_height: usize,

    /// Initial B–G line delta for color mode
    #[arg(long, default_value_t = 0)]
    bg_delta: i32,

    /// Initial G–R line delta for color mode
    #[arg(long, default_value_t = 0)]
    gr_delta: i32,

    /// Auto-detect color channel deltas at startup
    #[arg(long, short = 'a')]
    auto_delta: bool,
}

// ---------------------------------------------------------------------------
// Pixel helpers
// ---------------------------------------------------------------------------

fn gray_pixel(value: u8) -> u32 {
    let v = value as u32;
    (v << 16) | (v << 8) | v
}

fn rgb_pixel(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) << 16 | (g as u32) << 8 | (b as u32)
}

const BG: u32 = 0x00333333;

// ---------------------------------------------------------------------------
// Display mode
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum DisplayMode {
    Grayscale,
    Color,
}

impl DisplayMode {
    fn toggle(self) -> Self {
        match self {
            DisplayMode::Grayscale => DisplayMode::Color,
            DisplayMode::Color => DisplayMode::Grayscale,
        }
    }

    fn label(self) -> &'static str {
        match self {
            DisplayMode::Grayscale => "Gray",
            DisplayMode::Color => "Color",
        }
    }
}

// ---------------------------------------------------------------------------
// Image geometry
// ---------------------------------------------------------------------------

const BYTES_PER_PIXEL: usize = 6;

#[derive(Clone, Copy)]
struct ImageGeometry {
    pixel_width: usize,
    total_rows: usize,
}

/// Derive the TGCK CSV path from a binary capture file path.
fn tgck_path(bin_path: &Path) -> PathBuf {
    let stem = bin_path.file_stem().unwrap_or_default().to_string_lossy();
    bin_path.with_file_name(format!("{}_tgck.csv", stem))
}

/// Load TGCK line-end offsets from CSV.
/// The CSV has 8 columns:
///   rising_byte_index, rising_timestamp,
///   falling_byte_index, falling_timestamp,
///   first_clock_rising_byte_index, first_clock_rising_timestamp,
///   first_clock_falling_byte_index, first_clock_falling_timestamp
/// Uses `falling_byte_index` (third column) as line start offsets.
/// The falling edge lands consistently on a pixel boundary with no quantization
/// jitter, unlike the rising edge which can be off by ±1 byte.
fn load_tgck(path: &Path) -> Option<Vec<usize>> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut offsets: Vec<usize> = Vec::new();
    for line in content.lines().skip(1) {
        let mut cols = line.split(',');
        cols.next(); // rising_byte_index
        cols.next(); // rising_timestamp
        if let Some(idx) = cols.next()
            && let Ok(v) = idx.trim().parse::<usize>()
        {
            offsets.push(v);
        }
    }
    if offsets.len() >= 2 {
        Some(offsets)
    } else {
        None
    }
}

/// Compute line width in bytes from TGCK offsets (median of consecutive differences).
fn width_from_tgck(offsets: &[usize]) -> Option<usize> {
    if offsets.len() < 2 {
        return None;
    }
    let mut diffs: Vec<usize> = offsets
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    diffs.sort_unstable();
    Some(diffs[diffs.len() / 2])
}

/// Read a raw 16-bit channel value at (row, col). ch: 0=G, 1=B, 2=R.
fn read_channel_raw(data: &[u8], line_starts: &[usize], row: usize, col: usize, ch: usize) -> u16 {
    let pixel_offset = line_starts[row] + col * BYTES_PER_PIXEL;
    let hi_offset = pixel_offset + 2 * ch + 1;
    let lo_offset = pixel_offset + 2 * ch;
    if lo_offset < data.len() && hi_offset < data.len() {
        ((data[hi_offset] as u16) << 8) | (data[lo_offset] as u16)
    } else {
        0
    }
}

/// Find the optimal line delta between two color channels using cross-correlation
/// of row-to-row derivatives.
///
/// Samples columns across the image, builds per-row average signals for each
/// channel, differentiates them (row-to-row differences) to focus on edge
/// alignment rather than slow brightness gradients, then finds the delta
/// (shift of `ch_b` relative to `ch_a`) that maximizes the normalized
/// cross-correlation.
///
/// `max_delta` is the maximum absolute offset to search.
/// Returns `(best_delta, best_correlation)`.
fn find_channel_delta(
    data: &[u8],
    line_starts: &[usize],
    pixel_width: usize,
    ch_a: usize,
    ch_b: usize,
    max_delta: i32,
) -> (i32, f64) {
    let total_rows = line_starts.len().saturating_sub(1);
    if total_rows < (2 * max_delta as usize + 2) || pixel_width == 0 {
        return (0, 0.0);
    }

    // Sample ~64 evenly-spaced columns (skip edges that may have artifacts)
    let margin = pixel_width / 20;
    let usable = pixel_width.saturating_sub(2 * margin).max(1);
    let num_cols = 64.min(usable);
    let step = usable / num_cols;
    let cols: Vec<usize> = (0..num_cols).map(|i| margin + i * step).collect();

    // Build per-row mean signal for each channel
    let mut raw_a = vec![0.0f64; total_rows];
    let mut raw_b = vec![0.0f64; total_rows];
    let inv = 1.0 / cols.len() as f64;
    for row in 0..total_rows {
        let mut sum_a = 0.0f64;
        let mut sum_b = 0.0f64;
        for &col in &cols {
            sum_a += read_channel_raw(data, line_starts, row, col, ch_a) as f64;
            sum_b += read_channel_raw(data, line_starts, row, col, ch_b) as f64;
        }
        raw_a[row] = sum_a * inv;
        raw_b[row] = sum_b * inv;
    }

    // Differentiate: use row-to-row differences to focus on edges
    let n_deriv = total_rows - 1;
    let mut sig_a = vec![0.0f64; n_deriv];
    let mut sig_b = vec![0.0f64; n_deriv];
    for i in 0..n_deriv {
        sig_a[i] = raw_a[i + 1] - raw_a[i];
        sig_b[i] = raw_b[i + 1] - raw_b[i];
    }

    let mut best_delta = 0i32;
    let mut best_corr = f64::NEG_INFINITY;
    let mut all_corrs: Vec<(i32, f64)> = Vec::new();

    for d in -max_delta..=max_delta {
        // Overlap range: a_row in [a_start..a_end), b_row = a_row + d
        let a_start = 0.max(-d) as usize;
        let a_end = (n_deriv as i32).min(n_deriv as i32 - d) as usize;
        let n = a_end.saturating_sub(a_start);
        if n < 10 {
            continue;
        }
        let inv_n = 1.0 / n as f64;

        // Compute means
        let mut mean_a = 0.0f64;
        let mut mean_b = 0.0f64;
        for a_row in a_start..a_end {
            mean_a += sig_a[a_row];
            mean_b += sig_b[(a_row as i32 + d) as usize];
        }
        mean_a *= inv_n;
        mean_b *= inv_n;

        // Compute normalized cross-correlation
        let mut num = 0.0f64;
        let mut den_a = 0.0f64;
        let mut den_b = 0.0f64;
        for a_row in a_start..a_end {
            let da = sig_a[a_row] - mean_a;
            let db = sig_b[(a_row as i32 + d) as usize] - mean_b;
            num += da * db;
            den_a += da * da;
            den_b += db * db;
        }
        let denom = (den_a * den_b).sqrt();
        let corr = if denom > 1e-12 { num / denom } else { 0.0 };

        all_corrs.push((d, corr));

        if corr > best_corr {
            best_corr = corr;
            best_delta = d;
        }
    }

    // Print correlation profile around the peak
    println!(
        "  Cross-correlation ch{}↔ch{} (top 5 near peak):",
        ch_a, ch_b
    );
    all_corrs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for &(d, c) in all_corrs.iter().take(5) {
        println!(
            "    delta={:+4}: corr={:.6}{}",
            d,
            c,
            if d == best_delta { " ← best" } else { "" }
        );
    }

    (best_delta, best_corr)
}

/// Auto-detect both B-G and G-R deltas.
///
/// Cross-correlation finds `d` where `G(row)` best matches `ch(row + d)`.
/// In `decode_color`: `b_row = row - bg_delta`, `r_row = row + gr_delta`.
/// So: `bg_delta = -d_bg`, `gr_delta = d_gr`.
fn auto_detect_deltas(
    data: &[u8],
    line_starts: &[usize],
    pixel_width: usize,
    max_delta: i32,
) -> (i32, f64, i32, f64) {
    // ch 0=G, 1=B, 2=R
    println!("Auto-detecting channel deltas...");
    let (bg_raw, bg_corr) = find_channel_delta(data, line_starts, pixel_width, 0, 1, max_delta);
    let (gr_raw, gr_corr) = find_channel_delta(data, line_starts, pixel_width, 0, 2, max_delta);
    // Negate bg because decode_color uses (row - bg_delta) for B
    (-bg_raw, bg_corr, gr_raw, gr_corr)
}

// ---------------------------------------------------------------------------
// Phase 1: Decode — raw bytes → pixel image (only when data params change)
// ---------------------------------------------------------------------------

/// Parameters that determine the decoded image content.
/// When any of these change, we must re-decode.
#[derive(Clone, Copy, PartialEq, Eq)]
struct DecodeParams {
    display_mode: DisplayMode,
    bg_delta: i32,
    gr_delta: i32,
    show_r: bool,
    show_g: bool,
    show_b: bool,
    /// Brightness gain: stored as f32::to_bits() for Eq compatibility.
    /// Apply as: (val16 as f32 * f32::from_bits(gain_bits)).clamp(0.0, 255.0) as u8
    gain_bits: u32,
    /// When true, apply sqrt gamma after linear scaling (perceptual brightness).
    gamma: bool,
}

/// Decoded image cache
struct DecodedImage {
    params: Option<DecodeParams>,
    geo: ImageGeometry,
    pixels: Vec<u32>,
}

impl DecodedImage {
    fn new() -> Self {
        Self {
            params: None,
            geo: ImageGeometry {
                pixel_width: 0,
                total_rows: 0,
            },
            pixels: Vec::new(),
        }
    }

    /// Force next update() to re-decode even if params haven't changed.
    fn invalidate(&mut self) {
        self.params = None;
    }

    /// Re-decode only if params changed. Returns true if decode happened.
    fn update(
        &mut self,
        data: &[u8],
        line_starts: &[usize],
        pixel_width: usize,
        params: DecodeParams,
    ) -> bool {
        if self.params.as_ref() == Some(&params) {
            return false;
        }
        self.params = Some(params);

        let total_rows = line_starts.len().saturating_sub(1);
        let w = pixel_width;
        let h = total_rows;
        self.geo = ImageGeometry {
            pixel_width: w,
            total_rows: h,
        };
        self.pixels.resize(w * h, 0);

        match params.display_mode {
            DisplayMode::Grayscale => {
                for row in 0..h {
                    for col in 0..w {
                        self.pixels[row * w + col] = decode_grayscale(
                            data,
                            line_starts,
                            row,
                            col,
                            f32::from_bits(params.gain_bits),
                            params.gamma,
                        );
                    }
                }
            }
            DisplayMode::Color => {
                for row in 0..h {
                    for col in 0..w {
                        self.pixels[row * w + col] = decode_color(
                            data,
                            line_starts,
                            row,
                            col,
                            params.bg_delta,
                            params.gr_delta,
                            params.show_r,
                            params.show_g,
                            params.show_b,
                            f32::from_bits(params.gain_bits),
                            params.gamma,
                        );
                    }
                }
            }
        }

        true
    }

    /// Get pixel at (row, col) or background color
    fn get(&self, row: usize, col: usize, bg: u32) -> u32 {
        if row < self.geo.total_rows && col < self.geo.pixel_width {
            self.pixels[row * self.geo.pixel_width + col]
        } else {
            bg
        }
    }
}

// ---------------------------------------------------------------------------
// Pixel decoders — (data, row, col) → u32 color
// ---------------------------------------------------------------------------

/// Read one channel (0=G, 1=B, 2=R) from the given row/col as a 16-bit value.
/// 6-byte pixel layout: [G-hi, G-lo, B-hi, B-lo, R-hi, R-lo].
/// `gain` multiplies the raw u16 value; result is clamped to 0–255.
/// When `gamma` is true, sqrt is applied after scaling for perceptual brightness.
fn read_channel(
    data: &[u8],
    line_starts: &[usize],
    row: usize,
    col: usize,
    ch: usize,
    gain: f32,
    gamma: bool,
) -> u8 {
    let pixel_offset = line_starts[row] + col * BYTES_PER_PIXEL;
    let hi_offset = pixel_offset + 2 * ch + 1;
    let lo_offset = pixel_offset + 2 * ch;
    let val16 = if lo_offset < data.len() {
        ((data[hi_offset] as u16) << 8) | (data[lo_offset] as u16)
    } else if hi_offset < data.len() {
        (data[hi_offset] as u16) << 8
    } else {
        0
    };
    let linear = (val16 as f32 * gain).clamp(0.0, 255.0);
    if gamma {
        ((linear / 255.0).sqrt() * 255.0) as u8
    } else {
        linear as u8
    }
}

// R 2 5
// G -2 (4) 1
// B 0 3

fn decode_grayscale(
    data: &[u8],
    line_starts: &[usize],
    row: usize,
    col: usize,
    gain: f32,
    gamma: bool,
) -> u32 {
    let b = read_channel(data, line_starts, row, col, 1, gain, gamma) as f64;
    let g = read_channel(data, line_starts, row, col, 0, gain, gamma) as f64;
    let r = read_channel(data, line_starts, row, col, 2, gain, gamma) as f64;
    let lum = (0.299 * r + 0.587 * g + 0.114 * b) as u8;
    gray_pixel(lum)
}

#[allow(clippy::too_many_arguments)]
fn decode_color(
    data: &[u8],
    line_starts: &[usize],
    row: usize,
    col: usize,
    bg_delta: i32,
    gr_delta: i32,
    show_r: bool,
    show_g: bool,
    show_b: bool,
    gain: f32,
    gamma: bool,
) -> u32 {
    let total_rows = line_starts.len().saturating_sub(1);
    let b_row = (row as i32 - bg_delta).clamp(0, total_rows as i32 - 1) as usize;
    let g_row = row;
    let r_row = (row as i32 + gr_delta).clamp(0, total_rows as i32 - 1) as usize;

    let b = if show_b {
        read_channel(data, line_starts, b_row, col, 1, gain, gamma)
    } else {
        0
    };
    let g = if show_g {
        read_channel(data, line_starts, g_row, col, 0, gain, gamma)
    } else {
        0
    };
    let r = if show_r {
        read_channel(data, line_starts, r_row, col, 2, gain, gamma)
    } else {
        0
    };

    rgb_pixel(r, g, b)
}

// ---------------------------------------------------------------------------
// Phase 2: Blit — cached image → window framebuffer (pan/zoom only)
//
// When zoomed in (scale >= 1): nearest-neighbor point sampling.
// When zoomed out (scale < 1): box-average over source pixels that map
// to each screen pixel, giving proper downscaling without aliasing.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn blit(
    image: &DecodedImage,
    framebuf: &mut [u32],
    win_width: usize,
    win_height: usize,
    scroll_row: f64,
    scroll_col: f64,
    zoom: f64,
    bg: u32,
) {
    let geo = &image.geo;
    let scale = if geo.pixel_width == 0 {
        1.0
    } else {
        win_width as f64 / (geo.pixel_width as f64 * zoom)
    };

    let inv_scale = 1.0 / scale; // source pixels per screen pixel

    if scale >= 1.0 {
        // Zoomed in or 1:1: simple point sampling
        for dy in 0..win_height {
            let src_row = (dy as f64 * inv_scale + scroll_row) as usize;
            for dx in 0..win_width {
                let src_col = (dx as f64 * inv_scale + scroll_col) as usize;
                framebuf[dy * win_width + dx] = image.get(src_row, src_col, bg);
            }
        }
    } else {
        // Zoomed out: box-average over the source region that maps to each screen pixel
        for dy in 0..win_height {
            let src_y0 = dy as f64 * inv_scale + scroll_row;
            let src_y1 = (dy + 1) as f64 * inv_scale + scroll_row;
            let row0 = src_y0 as usize;
            let row1 = (src_y1 as usize).min(geo.total_rows);

            for dx in 0..win_width {
                let src_x0 = dx as f64 * inv_scale + scroll_col;
                let src_x1 = (dx + 1) as f64 * inv_scale + scroll_col;
                let col0 = src_x0 as usize;
                let col1 = (src_x1 as usize).min(geo.pixel_width);

                if row0 >= geo.total_rows || col0 >= geo.pixel_width {
                    framebuf[dy * win_width + dx] = bg;
                    continue;
                }

                let mut r_sum: u32 = 0;
                let mut g_sum: u32 = 0;
                let mut b_sum: u32 = 0;
                let mut count: u32 = 0;

                for sr in row0..row1 {
                    let row_base = sr * geo.pixel_width;
                    for sc in col0..col1 {
                        let px = image.pixels[row_base + sc];
                        r_sum += (px >> 16) & 0xFF;
                        g_sum += (px >> 8) & 0xFF;
                        b_sum += px & 0xFF;
                        count += 1;
                    }
                }

                framebuf[dy * win_width + dx] = match (
                    r_sum.checked_div(count),
                    g_sum.checked_div(count),
                    b_sum.checked_div(count),
                ) {
                    (Some(r), Some(g), Some(b)) => rgb_pixel(r as u8, g as u8, b as u8),
                    _ => bg,
                };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("Memory-mapping {}...", args.file.display());
    let file = File::open(&args.file)?;
    let data = unsafe { Mmap::map(&file)? };
    println!(
        "Mapped {} bytes ({:.1} MB)",
        data.len(),
        data.len() as f64 / 1_048_576.0
    );

    // Load TGCK line boundaries (auto-detected from bin filename).
    // rising_byte_index is the START of each line: the TGCK rising edge triggers the
    // transfer gate, beginning pixel readout for that scan line.
    let tgck_csv = tgck_path(&args.file);
    let tgck_raw: Vec<usize> = load_tgck(&tgck_csv)
        .ok_or_else(|| format!("TGCK file not found or invalid: {}", tgck_csv.display()))?;
    let line_width_bytes = match args.width {
        Some(w) => w,
        None => width_from_tgck(&tgck_raw)
            .ok_or("Cannot auto-detect width: need at least 2 TGCK records (use --width)")?,
    };
    let pixel_width = line_width_bytes / BYTES_PER_PIXEL;
    let mut start_offset: i32 = 4;
    let mut line_starts: Vec<usize> = tgck_raw
        .iter()
        .map(|&v| (v as i64 + start_offset as i64).max(0) as usize)
        .collect();
    let total_rows = line_starts.len().saturating_sub(1);
    println!(
        "{} lines, {} bytes/line ({} pixels wide), first line at byte {}",
        total_rows,
        line_width_bytes,
        pixel_width,
        line_starts.first().copied().unwrap_or(0)
    );

    let win_width = args.win_width;
    let win_height = args.win_height;
    let mut scroll_row: f64 = 0.0;
    let mut scroll_col: f64 = 0.0;
    let mut zoom: f64 = 1.0;
    let mut display_mode = DisplayMode::Color;
    let mut bg_delta: i32 = args.bg_delta;
    let mut gr_delta: i32 = args.gr_delta;

    // Auto-detect deltas at startup if requested
    if args.auto_delta {
        let (bg, bg_corr, gr, gr_corr) = auto_detect_deltas(&data, &line_starts, pixel_width, 80);
        println!(
            "Auto-detected deltas: B–G={} (corr={:.4}), G–R={} (corr={:.4})",
            bg, bg_corr, gr, gr_corr
        );
        bg_delta = bg;
        gr_delta = gr;
    }
    let mut deltas_locked = true;
    let mut show_r = true;
    let mut show_g = true;
    let mut show_b = true;
    // Default gain: 1/96 — between shift-6 (1/64) and shift-7 (1/128), a good starting point.
    let mut gain: f32 = 1.0 / 96.0;
    let mut use_gamma = false;
    let mut framebuf = vec![0u32; win_width * win_height];
    let mut image = DecodedImage::new();

    // Track what triggers decode vs just blit
    let mut needs_decode = true;
    let mut needs_blit = true;

    let mut window = Window::new(
        "CCD Viewer",
        win_width,
        win_height,
        WindowOptions {
            resize: false,
            ..WindowOptions::default()
        },
    )?;

    window.set_target_fps(60);

    while window.is_open() && !window.is_key_down(Key::Escape) {
        // --- Input handling ---

        // Pan with arrow keys → blit only
        let shift = window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift);
        if window.is_key_pressed(Key::Down, KeyRepeat::Yes) {
            scroll_row += if shift { 1.0 } else { 10.0 };
            needs_blit = true;
        }
        if window.is_key_pressed(Key::Up, KeyRepeat::Yes) {
            scroll_row = (scroll_row - if shift { 1.0 } else { 10.0 }).max(0.0);
            needs_blit = true;
        }
        if window.is_key_pressed(Key::Right, KeyRepeat::Yes) {
            scroll_col += if shift { 10.0 } else { 100.0 };
            needs_blit = true;
        }
        if window.is_key_pressed(Key::Left, KeyRepeat::Yes) {
            scroll_col = (scroll_col - if shift { 10.0 } else { 100.0 }).max(0.0);
            needs_blit = true;
        }
        if window.is_key_pressed(Key::PageDown, KeyRepeat::Yes) {
            scroll_row += 100.0;
            needs_blit = true;
        }
        if window.is_key_pressed(Key::PageUp, KeyRepeat::Yes) {
            scroll_row = (scroll_row - 100.0).max(0.0);
            needs_blit = true;
        }

        // Home / End → blit only
        if window.is_key_pressed(Key::Home, KeyRepeat::No) {
            scroll_row = 0.0;
            scroll_col = 0.0;
            needs_blit = true;
        }
        if window.is_key_pressed(Key::End, KeyRepeat::No) {
            let scale = if pixel_width == 0 {
                1.0
            } else {
                win_width as f64 / (pixel_width as f64 * zoom)
            };
            let vis_rows = win_height as f64 / scale;
            scroll_row = (total_rows as f64 - vis_rows).max(0.0);
            needs_blit = true;
        }

        // Display mode → decode
        if window.is_key_pressed(Key::G, KeyRepeat::No) {
            display_mode = display_mode.toggle();
            println!("Mode: {}", display_mode.label());
            needs_decode = true;
        }

        // D: adjust both deltas together → decode
        if window.is_key_pressed(Key::D, KeyRepeat::Yes) {
            if shift {
                bg_delta -= 1;
                gr_delta -= 1;
            } else {
                bg_delta += 1;
                gr_delta += 1;
            }
            println!(
                "B–G delta: {}, G–R delta: {} (locked={})",
                bg_delta, gr_delta, deltas_locked
            );
            needs_decode = true;
        }

        // B–G line delta → decode
        if window.is_key_pressed(Key::B, KeyRepeat::Yes) {
            if shift {
                bg_delta -= 1;
            } else {
                bg_delta += 1;
            }
            if deltas_locked {
                gr_delta = bg_delta;
            }
            println!(
                "B–G delta: {}, G–R delta: {} (locked={})",
                bg_delta, gr_delta, deltas_locked
            );
            needs_decode = true;
        }

        // G–R line delta → decode
        if window.is_key_pressed(Key::R, KeyRepeat::Yes) {
            if shift {
                gr_delta -= 1;
            } else {
                gr_delta += 1;
            }
            if deltas_locked {
                bg_delta = gr_delta;
            }
            println!(
                "B–G delta: {}, G–R delta: {} (locked={})",
                bg_delta, gr_delta, deltas_locked
            );
            needs_decode = true;
        }

        // L: toggle lock → when locking, set gr_delta = bg_delta
        if window.is_key_pressed(Key::L, KeyRepeat::No) {
            deltas_locked = !deltas_locked;
            if deltas_locked {
                gr_delta = bg_delta;
                needs_decode = true;
            }
            println!(
                "Deltas locked: {} (bg={}, gr={})",
                deltas_locked, bg_delta, gr_delta
            );
        }

        // Channel toggles → decode
        if window.is_key_pressed(Key::F5, KeyRepeat::No) {
            show_r = !show_r;
            println!("Channels: R={} G={} B={}", show_r, show_g, show_b);
            needs_decode = true;
        }
        if window.is_key_pressed(Key::F6, KeyRepeat::No) {
            show_g = !show_g;
            println!("Channels: R={} G={} B={}", show_r, show_g, show_b);
            needs_decode = true;
        }
        if window.is_key_pressed(Key::F7, KeyRepeat::No) {
            show_b = !show_b;
            println!("Channels: R={} G={} B={}", show_r, show_g, show_b);
            needs_decode = true;
        }

        // Start byte offset → decode
        if window.is_key_pressed(Key::LeftBracket, KeyRepeat::No) {
            start_offset -= 1;
            line_starts = tgck_raw
                .iter()
                .map(|&v| (v as i64 + start_offset as i64).max(0) as usize)
                .collect();
            println!("Start offset: {}", start_offset);
            image.invalidate();
            needs_decode = true;
        }
        if window.is_key_pressed(Key::RightBracket, KeyRepeat::No) {
            start_offset += 1;
            line_starts = tgck_raw
                .iter()
                .map(|&v| (v as i64 + start_offset as i64).max(0) as usize)
                .collect();
            println!("Start offset: {}", start_offset);
            image.invalidate();
            needs_decode = true;
        }

        // Brightness (gain) → decode
        // Each S press is ¼ stop (×2^0.25 ≈ ×1.19). Shift+S goes darker.
        if window.is_key_pressed(Key::S, KeyRepeat::No) {
            if shift {
                gain /= 2f32.powf(0.25);
            } else {
                gain *= 2f32.powf(0.25);
            }
            println!("Gain: {:.5} (≈ shift {:.2})", gain, -(gain.log2()));
            needs_decode = true;
        }

        // Gamma toggle → decode
        if window.is_key_pressed(Key::Y, KeyRepeat::No) {
            use_gamma = !use_gamma;
            println!(
                "Gamma correction: {}",
                if use_gamma { "sqrt" } else { "linear" }
            );
            needs_decode = true;
        }

        // Auto-detect deltas → decode
        if window.is_key_pressed(Key::A, KeyRepeat::No) {
            let (bg, bg_corr, gr, gr_corr) =
                auto_detect_deltas(&data, &line_starts, pixel_width, 80);
            println!(
                "Auto-detected deltas: B–G={} (corr={:.4}), G–R={} (corr={:.4})",
                bg, bg_corr, gr, gr_corr
            );
            bg_delta = bg;
            gr_delta = gr;
            needs_decode = true;
        }

        // Zoom → blit only
        if window.is_key_pressed(Key::Equal, KeyRepeat::Yes) {
            zoom *= 1.25;
            needs_blit = true;
        }
        if window.is_key_pressed(Key::Minus, KeyRepeat::Yes) {
            zoom = (zoom / 1.25).max(0.01);
            needs_blit = true;
        }
        if window.is_key_pressed(Key::Key0, KeyRepeat::No) {
            zoom = 1.0;
            scroll_col = 0.0;
            needs_blit = true;
        }
        if window.is_key_pressed(Key::P, KeyRepeat::No) {
            zoom = win_width as f64 / pixel_width.max(1) as f64;
            needs_blit = true;
        }
        if window.is_key_pressed(Key::T, KeyRepeat::No) {
            zoom = win_width as f64 / (pixel_width.max(1) as f64 * 10.0);
            needs_blit = true;
        }
        for (key, level) in [
            (Key::Key1, 1.0),
            (Key::Key2, 2.0),
            (Key::Key3, 3.0),
            (Key::Key4, 4.0),
            (Key::Key5, 5.0),
            (Key::Key6, 6.0),
            (Key::Key7, 7.0),
            (Key::Key8, 8.0),
            (Key::Key9, 9.0),
        ] {
            if window.is_key_pressed(key, KeyRepeat::No) {
                zoom = level;
                needs_blit = true;
            }
        }

        // Print info
        if window.is_key_pressed(Key::W, KeyRepeat::No) {
            let geo = image.geo;
            let scale = win_width as f64 / (geo.pixel_width.max(1) as f64 * zoom);
            println!(
                "Width: {} bytes, Offset: {}, Pixels: {}×{}, Scroll: row={:.0} col={:.0}, Zoom: {:.2}x, Scale: {:.3} px/src",
                line_width_bytes,
                line_starts.first().copied().unwrap_or(0),
                geo.pixel_width,
                geo.total_rows,
                scroll_row,
                scroll_col,
                zoom,
                scale
            );
        }

        // --- Phase 1: Decode (only if data params changed) ---
        if needs_decode {
            let params = DecodeParams {
                display_mode,
                bg_delta,
                gr_delta,
                show_r,
                show_g,
                show_b,
                gain_bits: gain.to_bits(),
                gamma: use_gamma,
            };
            image.update(&data, &line_starts, pixel_width, params);
            needs_decode = false;
            needs_blit = true; // decode always implies re-blit
        }

        // --- Clamp scroll ---
        let geo = image.geo;
        let scale = if geo.pixel_width == 0 {
            1.0
        } else {
            win_width as f64 / (geo.pixel_width as f64 * zoom)
        };
        let vis_cols = win_width as f64 / scale;
        let vis_rows = win_height as f64 / scale;

        if scroll_row + vis_rows > geo.total_rows as f64 {
            scroll_row = (geo.total_rows as f64 - vis_rows).max(0.0);
        }
        let max_col = (geo.pixel_width as f64 - vis_cols).max(0.0);
        if scroll_col > max_col {
            scroll_col = max_col;
        }

        // --- Phase 2: Blit (pan/zoom from cached image) ---
        if needs_blit {
            let bg = BG;

            blit(
                &image,
                &mut framebuf,
                win_width,
                win_height,
                scroll_row,
                scroll_col,
                zoom,
                bg,
            );

            let channels = format!(
                "{}{}{}",
                if show_r { "R" } else { "_" },
                if show_g { "G" } else { "_" },
                if show_b { "B" } else { "_" },
            );
            let title = format!(
                "CCD Viewer | {} bg={} gr={} {} ch={} gain={:.4}{} | w={} off={} img={}×{} scroll=({:.0},{:.0}) zoom={:.2}x | [arrows] pan [+-] zoom [G] mode [D] delta [B] bg∆ [R] gr∆ [L] lock [S/Shift+S] brightness [Y] gamma [A] auto [F5-7] RGB [W] info",
                display_mode.label(),
                bg_delta,
                gr_delta,
                if deltas_locked { "🔒" } else { "🔓" },
                channels,
                gain,
                if use_gamma { " γ" } else { "" },
                line_width_bytes,
                line_starts.first().copied().unwrap_or(0),
                geo.pixel_width,
                geo.total_rows,
                scroll_col,
                scroll_row,
                zoom,
            );
            window.set_title(&title);
            needs_blit = false;
        }

        window
            .update_with_buffer(&framebuf, win_width, win_height)
            .unwrap();
    }

    Ok(())
}
