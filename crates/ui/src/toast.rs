//! Transient toasts (`docs/UI_UX_IMPROVEMENT_PLAN.md` Phase 4.2) — the single
//! place `App` reports one-off events (file loaded/saved, node(s)
//! copied/pasted, a live edit applied or failed) without them pinning a
//! toolbar label forever. Ongoing *state* (a run that needs a restart to
//! pick up an edit, the current compile-error summary) stays in the toolbar
//! next to Run/Stop instead — that's not a toast's job.

use egui::{Color32, Context};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    Info,
    Error,
}

struct Toast {
    text: String,
    severity: Severity,
    /// Stamped lazily on the first `show()` after the toast is pushed, so
    /// callers never need to thread `egui::Context` through just to report
    /// an event — only `show()` (called once per frame) needs it.
    created: Option<f64>,
    dismissed: bool,
}

/// Info toasts fade out this many seconds after appearing.
const FADE_AFTER_S: f64 = 4.0;
/// The fade is a linear alpha ramp over this final stretch.
const FADE_RAMP_S: f64 = 1.0;

#[derive(Default)]
pub struct Toasts(Vec<Toast>);

impl Toasts {
    /// Fades out on its own after ~4s.
    pub fn info(&mut self, text: impl Into<String>) {
        self.0.push(Toast {
            text: text.into(),
            severity: Severity::Info,
            created: None,
            dismissed: false,
        });
    }

    /// Persists until dismissed (✕) or the toast stack scrolls it away.
    pub fn error(&mut self, text: impl Into<String>) {
        self.0.push(Toast {
            text: text.into(),
            severity: Severity::Error,
            created: None,
            dismissed: false,
        });
    }

    /// Draws the toast stack bottom-right and prunes expired/dismissed
    /// entries. Call once per frame; cheap no-op when nothing's pending.
    pub fn show(&mut self, ctx: &Context) {
        if self.0.is_empty() {
            return;
        }
        let now = ctx.input(|i| i.time);
        for toast in &mut self.0 {
            if toast.created.is_none() {
                toast.created = Some(now);
            }
        }
        self.0.retain(|toast| {
            !toast.dismissed
                && (toast.severity == Severity::Error
                    || now - toast.created.unwrap_or(now) < FADE_AFTER_S)
        });
        if self.0.is_empty() {
            return;
        }

        let mut dismiss: Option<usize> = None;
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    for (index, toast) in self.0.iter().enumerate().rev() {
                        let elapsed = now - toast.created.unwrap_or(now);
                        let alpha = if toast.severity == Severity::Error {
                            1.0
                        } else {
                            ((FADE_AFTER_S - elapsed) / FADE_RAMP_S).clamp(0.0, 1.0) as f32
                        };
                        let (bg, fg) = match toast.severity {
                            Severity::Info => (
                                Color32::from_rgba_unmultiplied(45, 45, 45, (alpha * 235.0) as u8),
                                Color32::from_rgba_unmultiplied(
                                    220,
                                    220,
                                    220,
                                    (alpha * 255.0) as u8,
                                ),
                            ),
                            Severity::Error => {
                                (Color32::from_rgb(92, 38, 38), Color32::from_rgb(240, 210, 210))
                            }
                        };
                        egui::Frame::new()
                            .fill(bg)
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin {
                                left: 10,
                                right: 8,
                                top: 6,
                                bottom: 6,
                            })
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.colored_label(fg, &toast.text);
                                    if toast.severity == Severity::Error
                                        && ui
                                            .add(egui::Button::new("✕").small().frame(false))
                                            .clicked()
                                    {
                                        dismiss = Some(index);
                                    }
                                });
                            });
                        ui.add_space(4.0);
                    }
                });
            });
        if let Some(index) = dismiss {
            self.0[index].dismissed = true;
        }

        let any_fading = self.0.iter().any(|t| t.severity == Severity::Info);
        ctx.request_repaint_after(std::time::Duration::from_millis(if any_fading {
            16
        } else {
            250
        }));
    }
}
