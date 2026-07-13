//! Placeholder waveform data shown in the logic analyzer before any capture
//! is opened or pipeline run — purely cosmetic so the view isn't blank on
//! first launch. The viewer itself has no notion of "demo" data; it just
//! renders whatever [`ChannelSignal`]s it's handed.

use logic_analyzer_viewer::ChannelSignal;

pub fn channels() -> Vec<ChannelSignal> {
    let mut channels = vec![uart_signal(0, "serial.rx", b"HELLO\n")];
    for index in 1..10 {
        let period = match index {
            1 => 90.0,
            2 => 135.0,
            3 => 260.0,
            6 => 42.0,
            7 => 28.0,
            _ => 220.0 + index as f64 * 35.0,
        };
        let offset = index as f64 * 11.0;
        channels.push(square_wave_signal(
            index,
            index.to_string(),
            period,
            offset,
            index % 3 == 0,
        ));
    }
    channels
}

fn uart_signal(index: usize, name: &str, bytes: &[u8]) -> ChannelSignal {
    const BAUD: f64 = 115_200.0;
    const FIRST_START_NS: u64 = 60_000;
    let bit_ns = (1_000_000_000.0 / BAUD).round() as u64;
    let mut transitions = Vec::new();
    let mut raw_level = true;
    let mut time_ns = FIRST_START_NS;

    for &byte in bytes {
        let frame_start = time_ns;
        let mut bits = Vec::with_capacity(10);
        bits.push(false);
        for bit in 0..8 {
            bits.push(((byte >> bit) & 1) == 1);
        }
        bits.push(true);

        for (bit_index, bit_value) in bits.into_iter().enumerate() {
            let bit_time_ns = frame_start + bit_index as u64 * bit_ns;
            if raw_level != bit_value {
                raw_level = bit_value;
                transitions.push((bit_time_ns as f64 / 1_000.0, raw_level));
            }
        }
        time_ns = frame_start + 10 * bit_ns;
    }

    ChannelSignal {
        index,
        name: name.to_owned(),
        initial: true,
        transitions,
    }
}

fn square_wave_signal(
    index: usize,
    name: String,
    period_us: f64,
    offset_us: f64,
    initial: bool,
) -> ChannelSignal {
    let mut transitions = Vec::new();
    let mut value = initial;
    let mut time = offset_us.max(0.0);
    while time <= 60_000.0 {
        value = !value;
        transitions.push((time, value));
        time += period_us * 0.5;
    }

    ChannelSignal {
        index,
        name,
        initial,
        transitions,
    }
}
