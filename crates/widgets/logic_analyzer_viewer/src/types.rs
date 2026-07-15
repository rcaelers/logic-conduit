use egui::{Color32, Pos2, Rect};

use signal_processing::CaptureMetadata;

use crate::lanes::ViewerLaneGroupId;

/// Color profile for the viewer. DSView (Tango-based channel colors, bright
/// traces) is the default; Classic is the viewer's original muted look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorProfile {
    DsView,
    Classic,
}

impl ColorProfile {
    pub(crate) const ALL: [Self; 2] = [Self::DsView, Self::Classic];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::DsView => "DSView",
            Self::Classic => "Classic",
        }
    }

    pub(crate) fn channel_color(self, index: usize) -> Color32 {
        const DSVIEW: [Color32; 8] = [
            Color32::from_rgb(80, 80, 80),   // grey
            Color32::from_rgb(143, 82, 2),   // brown
            Color32::from_rgb(204, 0, 0),    // red
            Color32::from_rgb(245, 121, 0),  // orange
            Color32::from_rgb(237, 212, 0),  // yellow
            Color32::from_rgb(115, 210, 22), // green
            Color32::from_rgb(52, 101, 164), // blue
            Color32::from_rgb(117, 80, 123), // violet
        ];
        const CLASSIC: [Color32; 8] = [
            Color32::from_rgb(210, 65, 65),
            Color32::from_rgb(210, 125, 45),
            Color32::from_rgb(215, 195, 45),
            Color32::from_rgb(80, 160, 85),
            Color32::from_rgb(70, 155, 190),
            Color32::from_rgb(95, 110, 205),
            Color32::from_rgb(155, 95, 185),
            Color32::from_rgb(180, 180, 180),
        ];
        match self {
            Self::DsView => DSVIEW[index % DSVIEW.len()],
            Self::Classic => CLASSIC[index % CLASSIC.len()],
        }
    }

    /// Waveform trace color: DSView draws bright, near-white traces.
    pub(crate) fn trace(self) -> Color32 {
        match self {
            Self::DsView => Color32::from_rgb(205, 205, 205),
            Self::Classic => Color32::from_rgb(135, 135, 135),
        }
    }
}

/// Identifies one display row, whether it's a raw channel (by its stable
/// capture index) or a derived lane (by its stable name —
/// `DerivedLanes::register` reuses a lane by name across runs, so this
/// survives a run restart the same way a channel index survives a
/// re-sample). The single ordering, drag, and rename mechanism in
/// `channel.rs` works on this — never on "channel" or "derived lane"
/// specifically — so the two interleave freely.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum RowKey {
    Channel(usize),
    Derived(ViewerLaneGroupId),
}

pub(crate) struct RowRenameState {
    pub(crate) key: RowKey,
    pub(crate) text: String,
    pub(crate) screen_pos: Pos2,
}

pub(crate) struct RowDragState {
    pub(crate) key: RowKey,
}

/// What a row's label needs, regardless of whether it's a channel or a
/// derived lane — the shared drawing code never branches on which.
pub(crate) struct RowLabel {
    pub(crate) name: String,
    pub(crate) badge_text: String,
    pub(crate) badge_color: Color32,
}

#[derive(Clone, Copy)]
pub(crate) struct AnalyzerLayout {
    pub(crate) header_rect: Rect,
    pub(crate) ruler_rect: Rect,
    pub(crate) labels_rect: Rect,
    pub(crate) wave_rect: Rect,
    pub(crate) row_height: f32,
    pub(crate) name_col_width: f32,
    pub(crate) badge_width: f32,
}

/// A vertical time marker (DSView-style "cursor"), added by double-clicking
/// the ruler and moved by dragging its flag or line.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TimeCursor {
    /// Display number (1-based). Freed numbers are reused, so a cursor's
    /// number — and the flag color derived from it — stays stable while
    /// other cursors come and go.
    pub(crate) number: usize,
    pub(crate) time_us: f64,
}

/// Per-frame outcome of cursor interaction, used to keep cursor drags from
/// also panning the view and ruler double-clicks from also fitting it.
#[derive(Default, Clone, Copy)]
pub(crate) struct CursorInput {
    /// Cursor being dragged or hovered, for highlighting.
    pub(crate) active: Option<usize>,
    pub(crate) blocks_pan: bool,
    pub(crate) ruler_double_click: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Transition {
    pub(crate) time_us: f64,
    pub(crate) value: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WaveformSegment {
    pub(crate) start_us: f64,
    pub(crate) end_us: f64,
    pub(crate) kind: WaveformSegmentKind,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum WaveformSegmentKind {
    Level { value: bool },
    Edge { before: bool, after: bool },
    Activity { first: bool, last: bool },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PulseMeasurement {
    pub(crate) channel_row: usize,
    pub(crate) value: bool,
    pub(crate) start_us: f64,
    pub(crate) end_us: f64,
    /// The bounding toggle on this side lies outside the examined window, so
    /// `start_us`/`end_us` is the window edge and Width is a lower bound.
    pub(crate) start_open: bool,
    pub(crate) end_open: bool,
    // `None` when the trace doesn't have a following transition to close a
    // full period (e.g. a single isolated pulse) — Width is still valid.
    pub(crate) period_end_us: Option<f64>,
    /// This row is a zero-width event lane (e.g. a `Markers` derived lane)
    /// reinterpreted as an alternating channel purely so the same hover/snap
    /// machinery applies (§`derived_markers_channel`) — there's no real
    /// "level" backing `value`, so drawing/reporting must not treat this
    /// like a real pulse (no high/low, no period/duty cycle).
    pub(crate) is_event: bool,
}

impl PulseMeasurement {
    pub(crate) fn width_us(self) -> f64 {
        self.end_us - self.start_us
    }

    pub(crate) fn period_us(self) -> Option<f64> {
        self.period_end_us
            .map(|period_end_us| period_end_us - self.start_us)
    }

    pub(crate) fn duty_cycle(self) -> Option<f64> {
        self.period_us()
            .map(|period_us| self.width_us() / period_us)
    }
}

/// Exact transitions pulled from the index for a window around a point of
/// interest, with the window bounds and the level at its start.
#[derive(Debug, Clone)]
pub(crate) struct ExactWindow {
    pub(crate) initial: bool,
    pub(crate) start_us: f64,
    pub(crate) end_us: f64,
    pub(crate) transitions: Vec<Transition>,
}

#[derive(Debug, Clone)]
pub(crate) struct CaptureInfo {
    // Rendered by the native capture worker's status text.
    #[allow(dead_code)]
    pub(crate) display_name: String,
    pub(crate) header: CaptureMetadata,
    pub(crate) duration_us: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct IndexBuildProgress {
    pub(crate) completed_roots: usize,
    pub(crate) total_roots: usize,
}

impl IndexBuildProgress {
    pub(crate) fn fraction(self) -> f32 {
        if self.total_roots == 0 {
            1.0
        } else {
            self.completed_roots as f32 / self.total_roots as f32
        }
    }
}
