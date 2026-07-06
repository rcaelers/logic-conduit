use egui::Color32;

/// Black on light badges, white on dark ones (grey, brown, blue, violet).
pub(crate) fn badge_text_color(background: Color32) -> Color32 {
    let luminance = 0.299 * background.r() as f32
        + 0.587 * background.g() as f32
        + 0.114 * background.b() as f32;
    if luminance < 128.0 {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

pub(crate) fn nice_step(raw: f64) -> f64 {
    if raw <= 0.0 {
        return 1.0;
    }

    let base = 10.0_f64.powf(raw.log10().floor());
    let fraction = raw / base;
    let nice = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * base
}

/// Formats a ruler tick label, choosing the unit from the tick's magnitude
/// and the decimal count from the tick spacing, so adjacent labels stay
/// distinguishable at any zoom (down to nanoseconds, even at large offsets).
pub(crate) fn format_time(us: f64, step_us: f64) -> String {
    let (scale, unit) = if us.abs() >= 1_000_000.0 {
        (1e-6, "s")
    } else if us.abs() >= 1_000.0 {
        (1e-3, "ms")
    } else if us.abs() >= 1.0 {
        (1.0, "µs")
    } else {
        (1e3, "ns")
    };
    let value = us * scale;
    let step = (step_us * scale).abs();
    let decimals = if step > 0.0 {
        (-step.log10().floor()).clamp(0.0, 9.0) as usize
    } else {
        0
    };
    format!("+{value:.decimals$}{unit}")
}

pub(crate) fn format_duration(us: f64) -> String {
    if us >= 1_000_000.0 {
        format!("{:.2} s", us / 1_000_000.0)
    } else if us >= 1_000.0 {
        format!("{:.2} ms", us / 1_000.0)
    } else if us >= 1.0 {
        format!("{:.2} µs", us)
    } else {
        format!("{:.0} ns", us * 1_000.0)
    }
}

/// Formats a time delta with at least 8 significant digits (DSView-style),
/// scaled to the natural unit.
pub(crate) fn format_delta(us: f64) -> String {
    let ns = us * 1_000.0;
    let (value, unit) = if ns.abs() < 1_000.0 {
        (ns, "ns")
    } else if us.abs() < 1_000.0 {
        (us, "µs")
    } else if us.abs() < 1_000_000.0 {
        (us / 1_000.0, "ms")
    } else {
        (us / 1_000_000.0, "s")
    };
    let integer_digits = if value.abs() < 1.0 {
        1
    } else {
        value.abs().log10().floor() as usize + 1
    };
    let decimals = 8_usize.saturating_sub(integer_digits);
    format!("+{value:.decimals$}{unit}")
}

pub(crate) fn format_frequency(period_us: f64) -> String {
    if period_us <= 0.0 {
        return "—".to_string();
    }

    let hz = 1_000_000.0 / period_us;
    if hz >= 1_000_000.0 {
        format!("{:.2}MHz", hz / 1_000_000.0)
    } else if hz >= 1_000.0 {
        format!("{:.2}kHz", hz / 1_000.0)
    } else {
        format!("{hz:.2}Hz")
    }
}
