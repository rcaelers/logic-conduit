use crate::types::{AnalyzerLayout, CaptureInfo, ExactWindow, PulseMeasurement, RowKey, Transition};
use crate::viewer::LogicAnalyzerViewer;
use crate::channel::channels_from_window;
use dsl::CaptureWaveformSegment;
use egui::Pos2;

impl LogicAnalyzerViewer {
    /// Samples the visible window from the index synchronously, so the drawn
    /// waveform always matches the current view exactly. Skipped when neither
    /// the view nor the viewport size changed since the last sampling. A
    /// no-op whenever `sampler` is `None` — always true on wasm, since
    /// nothing there ever constructs one (see `CaptureIndex`).
    pub(crate) fn sample_visible_window(&mut self, layout: AnalyzerLayout) {
        if layout.wave_rect.width() <= 1.0 {
            return;
        }
        let Some(capture) = self.capture_info.as_ref() else {
            return;
        };
        let samplerate_hz = capture.header.samplerate_hz;
        let (visible_start, visible_end) =
            visible_sample_range(capture, self.visible_start_us, self.visible_span_us);
        let target_points = layout.wave_rect.width().max(1.0).round() as usize;

        let key = (visible_start, visible_end, target_points);
        if self.sampled_key == Some(key) {
            return;
        }
        let requested_channels = self.requested_channel_order();
        let Some(sampler) = self.sampler.as_mut() else {
            return;
        };

        match sampler.sampled_window(
            &requested_channels,
            visible_start,
            visible_end,
            target_points,
        ) {
            Ok(window) => {
                let mut channels = channels_from_window(&window, samplerate_hz);
                self.apply_channel_names(&mut channels);
                self.apply_channel_order(&mut channels);
                self.channels = channels;
            }
            Err(err) => {
                self.status = format!("Could not read capture window: {err}");
            }
        }
        // Recorded even on failure so a persistent error does not retry every frame.
        self.sampled_key = Some(key);
    }

    /// Refreshes the pulse measurement for the current hover position.
    ///
    /// At low zoom the hovered channel is drawn from summarized `waveform`
    /// bands (see `draw_channel_waveform`), which record only first/last
    /// levels per band and don't carry individual edge times. To keep
    /// measurement accurate at any zoom, this pulls a small exact window
    /// straight from the index around the pointer instead of reusing the
    /// (possibly summarized) data backing the main view.
    pub(crate) fn sample_hover_measurement(&mut self, layout: AnalyzerLayout, pointer: Option<Pos2>) {
        let previous = self.hover_measurement.take();
        let Some(pointer) = pointer else {
            return;
        };
        let wave_rect = layout.wave_rect;
        if !wave_rect.contains(pointer) || wave_rect.width() <= 1.0 {
            return;
        }

        let row_height = layout.row_height;
        let channel_row = ((pointer.y - wave_rect.top()) / row_height).floor() as usize;
        let Some(channel) = self.channel_at_row(channel_row) else {
            return;
        };
        let time_us = self.x_to_time(wave_rect, pointer.x);

        // A measurement is a property of the run under the pointer, not of
        // the zoom level; while the pointer stays inside a fully resolved
        // run on the same row, the previous result is still exact.
        if let Some(previous) = previous
            && previous.channel_row == channel_row
            && !previous.start_open
            && !previous.end_open
            && time_us >= previous.start_us
            && time_us < previous.end_us
        {
            self.hover_measurement = Some(previous);
            return;
        }

        let visible_end_us = self.visible_start_us + self.visible_span_us;
        // Derived lanes (wherever they've been dragged to among the rows)
        // always measure from `channel.transitions` as already resolved by
        // `channel_at_row` — a bounded query against the lane's own
        // multi-resolution summary (`runtime::derived_index`), not the raw
        // `CaptureIndex` real channels use, so there's no further exact
        // refinement to fall through to below. A loaded capture's own
        // channels do take that index path, since even at zoom levels
        // where the visible window is exact, the run or its period may
        // close beyond the viewport.
        let row_is_indexed = matches!(self.row_order.get(channel_row), Some(RowKey::Channel(_)))
            && self.has_index_sampler();
        let measurement = if !row_is_indexed {
            pulse_measurement_from_window(
                &channel.transitions,
                channel.initial,
                self.visible_start_us,
                visible_end_us,
                time_us,
            )
        } else {
            let channel_index = channel.index;
            let Some(capture) = self.capture_info.as_ref() else {
                return;
            };
            let samplerate_hz = capture.header.samplerate_hz;
            let duration_us = capture.duration_us;

            let window = self.exact_transitions_around(wave_rect, channel_index, time_us, 24.0);
            let mut measurement = window.as_ref().and_then(|window| {
                pulse_measurement_from_window(
                    &window.transitions,
                    window.initial,
                    window.start_us,
                    window.end_us,
                    time_us,
                )
            });

            if let Some(measurement) = measurement.as_mut() {
                let pointer_sample = us_to_sample(time_us, samplerate_hz);
                let mut end_is_toggle = !measurement.end_open;
                // Resolve open sides exactly: search the index for the true
                // bounding toggles, however far away. The measured width
                // must never depend on the zoom level or query window size.
                if measurement.start_open {
                    measurement.start_open = false;
                    if let Some((sample, value)) =
                        self.prev_transition_at_or_before(channel_index, pointer_sample)
                    {
                        measurement.start_us = sample_to_us(sample, samplerate_hz);
                        measurement.value = value;
                    } else {
                        // The run reaches back to the start of the capture.
                        measurement.start_us = 0.0;
                    }
                }
                if measurement.end_open {
                    measurement.end_open = false;
                    if let Some((sample, _)) =
                        self.next_transition_after(channel_index, pointer_sample)
                    {
                        measurement.end_us = sample_to_us(sample, samplerate_hz);
                        end_is_toggle = true;
                    } else {
                        // The run reaches to the end of the capture.
                        measurement.end_us = duration_us;
                    }
                }
                // With the end edge exact, the period may still close beyond
                // the narrow window; one more search finds it.
                if measurement.period_end_us.is_none() && end_is_toggle {
                    let end_sample = us_to_sample(measurement.end_us, samplerate_hz);
                    if let Some((sample, _)) =
                        self.next_transition_after(channel_index, end_sample)
                    {
                        let period_end_us = sample_to_us(sample, samplerate_hz);
                        if period_end_us - measurement.start_us > measurement.width_us() {
                            measurement.period_end_us = Some(period_end_us);
                        }
                    }
                }
            }
            measurement
        };

        // An event lane has no real level, so "Period" (time back to a
        // matching level) doesn't mean anything — only the gap to the
        // neighboring event does, which `width_us` already is.
        let is_event = self.is_event_row(channel_row);
        self.hover_measurement = measurement.map(|measurement| PulseMeasurement {
            channel_row,
            is_event,
            period_end_us: if is_event { None } else { measurement.period_end_us },
            ..measurement
        });
    }

    pub(crate) fn has_index_sampler(&self) -> bool {
        self.sampler.is_some()
    }

    /// First toggle strictly after `sample`, searched across the whole
    /// capture.
    pub(crate) fn next_transition_after(
        &mut self,
        channel_index: usize,
        sample: u64,
    ) -> Option<(u64, bool)> {
        let total_samples = self.capture_info.as_ref()?.header.total_samples;
        self.find_transition(channel_index, sample, sample, total_samples, false)
    }

    /// Last toggle at or before `sample`, searched across the whole capture.
    pub(crate) fn prev_transition_at_or_before(
        &mut self,
        channel_index: usize,
        sample: u64,
    ) -> Option<(u64, bool)> {
        self.find_transition(channel_index, sample, 0, sample.saturating_add(1), true)
    }

    /// Locates the toggle nearest to `from_sample` within `[lo, hi)` —
    /// forward (first strictly after) or backward (last at or before) — by
    /// descending through the index's summary levels. Idle stretches are
    /// skipped wholesale, so even a bounding toggle many seconds away costs
    /// only a handful of coarse queries. Returns the toggle's sample and the
    /// level after it.
    pub(crate) fn find_transition(
        &mut self,
        channel_index: usize,
        from_sample: u64,
        lo: u64,
        hi: u64,
        backward: bool,
    ) -> Option<(u64, bool)> {
        if hi <= lo {
            return None;
        }
        const POINTS: usize = 1_024;
        let window = self
            .sampler
            .as_mut()?
            .sampled_window(&[channel_index], lo, hi, POINTS)
            .ok()?;
        let channel = window.channels.first()?;
        if window.sample_step == 1 {
            return if backward {
                channel
                    .transitions
                    .iter()
                    .rev()
                    .find(|transition| transition.sample <= from_sample)
            } else {
                channel
                    .transitions
                    .iter()
                    .find(|transition| transition.sample > from_sample)
            }
            .map(|transition| (transition.sample, transition.value));
        }

        let segments: Box<dyn Iterator<Item = &CaptureWaveformSegment>> = if backward {
            Box::new(channel.waveform.iter().rev())
        } else {
            Box::new(channel.waveform.iter())
        };
        for segment in segments {
            match *segment {
                CaptureWaveformSegment::Level { .. } => {}
                CaptureWaveformSegment::Edge { sample, after, .. } => {
                    if (backward && sample <= from_sample) || (!backward && sample > from_sample) {
                        return Some((sample, after));
                    }
                }
                CaptureWaveformSegment::Activity {
                    start_sample,
                    end_sample,
                    ..
                } => {
                    let relevant = if backward {
                        start_sample <= from_sample
                    } else {
                        end_sample > from_sample
                    };
                    if !relevant {
                        continue;
                    }
                    let sub_lo = start_sample.max(lo);
                    let sub_hi = end_sample.min(hi);
                    let found = if (sub_lo, sub_hi) == (lo, hi) {
                        // The summary could not split this range; bisect,
                        // trying the half nearest `from_sample` first.
                        let mid = lo + (hi - lo) / 2;
                        if backward {
                            self.find_transition(channel_index, from_sample, mid, hi, true)
                                .or_else(|| {
                                    self.find_transition(channel_index, from_sample, lo, mid, true)
                                })
                        } else {
                            self.find_transition(channel_index, from_sample, lo, mid, false)
                                .or_else(|| {
                                    self.find_transition(channel_index, from_sample, mid, hi, false)
                                })
                        }
                    } else {
                        self.find_transition(channel_index, from_sample, sub_lo, sub_hi, backward)
                    };
                    if found.is_some() {
                        return found;
                    }
                }
            }
        }
        None
    }

    /// Exact transitions for `channel_index` in an index-backed window
    /// around `time_us`, spanning `neighborhood_px` on-screen pixels to
    /// either side.
    ///
    /// The query is sized to the current zoom, so a signal dense enough to
    /// need band rendering still has its real edges captured. Bounded below
    /// (very zoomed in) and above (very zoomed out) to keep the raw scan
    /// cheap.
    pub(crate) fn exact_transitions_around(
        &mut self,
        wave_rect: egui::Rect,
        channel_index: usize,
        time_us: f64,
        neighborhood_px: f64,
    ) -> Option<ExactWindow> {
        let capture = self.capture_info.as_ref()?;
        let samplerate_hz = capture.header.samplerate_hz;
        let total_samples = capture.header.total_samples;
        let sampler = self.sampler.as_mut()?;

        let samples_per_pixel =
            (self.visible_span_us * samplerate_hz / 1_000_000.0 / wave_rect.width() as f64)
                .max(1.0);
        let half_window_samples =
            ((samples_per_pixel * neighborhood_px) as u64).clamp(4_096, 2_000_000);
        let center_sample = us_to_sample(time_us, samplerate_hz);
        let start_sample = center_sample.saturating_sub(half_window_samples);
        let end_sample = (center_sample + half_window_samples).min(total_samples);
        if end_sample <= start_sample {
            return None;
        }
        let window_samples = (end_sample - start_sample) as usize;

        let window = sampler
            .sampled_window(&[channel_index], start_sample, end_sample, window_samples)
            .ok()?;
        let sampled = window.channels.first()?;
        Some(ExactWindow {
            initial: sampled.initial,
            start_us: sample_to_us(start_sample, samplerate_hz),
            end_us: sample_to_us(end_sample, samplerate_hz),
            transitions: sampled
                .transitions
                .iter()
                .map(|transition| Transition {
                    time_us: sample_to_us(transition.sample, samplerate_hz),
                    value: transition.value,
                })
                .collect(),
        })
    }
}

/// Measures the run (high or low) under `time_us` from transitions covering
/// `[window_start_us, window_end_us]`. When a bounding toggle lies outside
/// the window, that side falls back to the window edge and is marked open,
/// so hovering the tail after the last visible toggle still measures.
pub(crate) fn pulse_measurement_from_window(
    transitions: &[Transition],
    initial: bool,
    window_start_us: f64,
    window_end_us: f64,
    time_us: f64,
) -> Option<PulseMeasurement> {
    let end_index = transitions.partition_point(|transition| transition.time_us <= time_us);
    let start = end_index
        .checked_sub(1)
        .and_then(|index| transitions.get(index));
    let end = transitions.get(end_index);

    let (start_us, start_open, value) = match start {
        Some(transition) => (transition.time_us, false, transition.value),
        None => (window_start_us, true, initial),
    };
    let (end_us, end_open) = match end {
        Some(transition) => (transition.time_us, false),
        None => (window_end_us, true),
    };

    let width_us = end_us - start_us;
    if width_us <= 0.0 {
        return None;
    }

    let period_end_us = if start_open || end_open {
        None
    } else {
        transitions
            .get(end_index + 1)
            .map(|period_end| period_end.time_us)
            .filter(|&period_end_us| period_end_us - start_us > width_us)
    };

    Some(PulseMeasurement {
        channel_row: 0,
        value,
        start_us,
        end_us,
        start_open,
        end_open,
        period_end_us,
        is_event: false,
    })
}

pub(crate) fn us_to_sample(time_us: f64, samplerate_hz: f64) -> u64 {
    (time_us.max(0.0) * samplerate_hz / 1_000_000.0).round() as u64
}

pub(crate) fn sample_to_us(sample: u64, samplerate_hz: f64) -> f64 {
    sample as f64 * 1_000_000.0 / samplerate_hz
}

pub(crate) fn visible_sample_range(capture: &CaptureInfo, start_us: f64, span_us: f64) -> (u64, u64) {
    let samplerate_hz = capture.header.samplerate_hz;
    let total_samples = capture.header.total_samples;
    let visible_start = us_to_sample(start_us, samplerate_hz).min(total_samples.saturating_sub(1));
    let visible_end =
        us_to_sample(start_us + span_us, samplerate_hz).clamp(visible_start + 1, total_samples);
    (visible_start, visible_end)
}
