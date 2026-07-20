//! UART decoder for single-line async serial words.
//!
//! Decodes one direction (RX *or* TX); instantiate twice for full duplex.
//! The bit clock is derived from the configured baud rate, not sampled:
//! bit centers are exact nanosecond math on the start edge, so decode
//! quality depends only on edges being captured faithfully.

use std::collections::VecDeque;

use tracing::{debug, trace};

use signal_processing::{
    InputPort, OutputPort, PortDirection, PortSchema, ProcessNode, Receiver, Sample, Trigger, Word,
    WorkError, WorkResult,
};

use super::types::BitOrder;

/// UART parity mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UartParity {
    #[default]
    None,
    Odd,
    Even,
    /// Parity bit always 1
    Mark,
    /// Parity bit always 0
    Space,
}

/// Number of stop bits
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UartStopBits {
    /// No stop bit (frames may be back-to-back)
    S0,
    S0_5,
    #[default]
    S1,
    S1_5,
    S2,
}

impl UartStopBits {
    fn bits(&self) -> f64 {
        match self {
            UartStopBits::S0 => 0.0,
            UartStopBits::S0_5 => 0.5,
            UartStopBits::S1 => 1.0,
            UartStopBits::S1_5 => 1.5,
            UartStopBits::S2 => 2.0,
        }
    }
}

/// UART decoder node
///
/// Input: `rx` — `Sample` edge stream of the transceive line
/// Outputs: `words` — one `Word` per frame (timestamped at the
///          start edge); `error` — `Trigger` per parity/framing error;
///          `bits` — one timed `Word` for every decoded data bit.
///          (the word is still emitted, annotation-style)
pub struct UartDecoder {
    name: String,
    baud: u64,
    data_bits: usize,
    parity: UartParity,
    check_parity: bool,
    stop_bits: UartStopBits,
    bit_order: BitOrder,
    invert: bool,

    input_buffer: VecDeque<Sample>,
    /// Start edges before this time are mid-frame remnants, not new frames.
    resume_after: u64,
    frames: u64,
    finished: bool,
}

impl UartDecoder {
    pub fn new(baud: u64, data_bits: usize) -> Self {
        assert!(baud > 0, "baud rate must be positive");
        assert!(
            (5..=9).contains(&data_bits),
            "data bits must be in 5..=9 (got {data_bits})"
        );
        Self {
            name: "uart_decoder".to_string(),
            baud,
            data_bits,
            parity: UartParity::default(),
            check_parity: false,
            stop_bits: UartStopBits::default(),
            bit_order: BitOrder::LsbFirst,
            invert: false,
            input_buffer: VecDeque::new(),
            resume_after: 0,
            frames: 0,
            finished: false,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_parity(mut self, parity: UartParity, check: bool) -> Self {
        self.parity = parity;
        self.check_parity = check;
        self
    }

    pub fn with_stop_bits(mut self, stop_bits: UartStopBits) -> Self {
        self.stop_bits = stop_bits;
        self
    }

    /// UART is LSB-first by default; MSB-first for exotic framings.
    pub fn with_bit_order(mut self, bit_order: BitOrder) -> Self {
        self.bit_order = bit_order;
        self
    }

    pub fn with_invert(mut self, invert: bool) -> Self {
        self.invert = invert;
        self
    }

    fn bit_ns(&self) -> f64 {
        1_000_000_000.0 / self.baud as f64
    }

    /// Line value at `timestamp` (same peek/putback approach as the SPI
    /// decoder). `None` when the channel is exhausted.
    fn value_at(channel: &mut Receiver<'_, Sample>, timestamp: u64) -> WorkResult<Option<bool>> {
        loop {
            let current = match channel.recv() {
                Ok(edge) => edge,
                Err(WorkError::Shutdown) => return Ok(None),
                Err(e) => return Err(e),
            };
            match channel.peek() {
                Ok(next) => {
                    if current.start_time_ns <= timestamp && timestamp < next.start_time_ns {
                        channel.put_back(current);
                        return Ok(Some(current.value));
                    }
                    // current has ended before `timestamp` — keep scanning
                }
                Err(WorkError::Shutdown) => {
                    // Last edge extends to infinity
                    if current.start_time_ns <= timestamp {
                        channel.put_back(current);
                        return Ok(Some(current.value));
                    }
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl ProcessNode for UartDecoder {
    fn name(&self) -> &str {
        &self.name
    }

    fn should_stop(&self) -> bool {
        self.finished
    }

    fn num_inputs(&self) -> usize {
        1
    }

    fn num_outputs(&self) -> usize {
        4
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<Sample>("rx", 0, PortDirection::Input)]
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![
            PortSchema::new::<Word>("words", 0, PortDirection::Output),
            PortSchema::new::<Trigger>("error", 1, PortDirection::Output),
            PortSchema::new::<Word>("bits", 2, PortDirection::Output),
            PortSchema::new::<Word>("frame", 3, PortDirection::Output),
        ]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let bit_ns = self.bit_ns();
        let data_bits = self.data_bits;
        let parity = self.parity;
        let check_parity = self.check_parity;
        let stop_bits = self.stop_bits;
        let bit_order = self.bit_order;
        let invert = self.invert;
        // Raw line level of a start bit (logical 0).
        let start_level = invert;

        let mut rx = inputs
            .first()
            .and_then(|port| port.get::<Sample>(&mut self.input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing rx input".to_string()))?;
        let words_out = outputs.first().and_then(|port| port.get::<Word>());
        // Optional; None when unconnected.
        let error_out = outputs.get(1).and_then(|port| port.get::<Trigger>());
        let bits_out = outputs.get(2).and_then(|port| port.get::<Word>());
        let frame_out = outputs.get(3).and_then(|port| port.get::<Word>());

        // ── 1. Hunt for a start edge past the previous frame ────────────
        let t0 = loop {
            let edge = rx.recv()?;
            if edge.value == start_level && edge.start_time_ns >= self.resume_after {
                // Put it back: this edge also carries the line level for the
                // first bit intervals when the data doesn't toggle.
                let t0 = edge.start_time_ns;
                rx.put_back(edge);
                break t0;
            }
            trace!(
                "[{}] skipping edge at {}ns (mid-frame or wrong level)",
                self.name, edge.start_time_ns
            );
        };

        let sample_time =
            |bit_index: f64| -> u64 { t0 + ((bit_index + 0.5) * bit_ns).round() as u64 };
        let logical = |raw: bool| -> bool { raw != invert };

        // ── 2. Sample data bits at their centers ────────────────────────
        let mut value: u64 = 0;
        let mut bits = Vec::with_capacity(data_bits);
        for i in 0..data_bits {
            let t = sample_time(1.0 + i as f64);
            let Some(raw) = Self::value_at(&mut rx, t)? else {
                debug!("[{}] channel exhausted mid-frame", self.name);
                self.finished = true;
                return Err(WorkError::Shutdown);
            };
            if logical(raw) {
                let bit_position = match bit_order {
                    BitOrder::LsbFirst => i,
                    BitOrder::MsbFirst => data_bits - 1 - i,
                };
                value |= 1 << bit_position;
            }
            bits.push(logical(raw));
        }

        // ── 3. Parity ────────────────────────────────────────────────────
        let parity_bits = usize::from(parity != UartParity::None);
        let mut frame_error = false;
        if parity != UartParity::None {
            let t = sample_time(1.0 + data_bits as f64);
            let Some(raw) = Self::value_at(&mut rx, t)? else {
                debug!("[{}] channel exhausted at parity bit", self.name);
                self.finished = true;
                return Err(WorkError::Shutdown);
            };
            let parity_bit = logical(raw);
            if check_parity {
                let ones = value.count_ones() as usize + usize::from(parity_bit);
                let ok = match parity {
                    UartParity::Odd => !ones.is_multiple_of(2),
                    UartParity::Even => ones.is_multiple_of(2),
                    UartParity::Mark => parity_bit,
                    UartParity::Space => !parity_bit,
                    UartParity::None => true,
                };
                if !ok {
                    debug!("[{}] parity error at {}ns", self.name, t0);
                    frame_error = true;
                }
            }
        }

        // ── 4. Stop bit level (logical 1 = mark) ─────────────────────────
        if stop_bits.bits() > 0.0 {
            // Center of the first stop interval (half a stop-bit in).
            let stop_start = 1.0 + (data_bits + parity_bits) as f64;
            let t = t0 + ((stop_start + stop_bits.bits().min(1.0) / 2.0) * bit_ns).round() as u64;
            let Some(raw) = Self::value_at(&mut rx, t)? else {
                debug!("[{}] channel exhausted at stop bit", self.name);
                self.finished = true;
                return Err(WorkError::Shutdown);
            };
            if !logical(raw) {
                debug!("[{}] framing error at {}ns", self.name, t0);
                frame_error = true;
            }
        }

        // ── 5. Emit ──────────────────────────────────────────────────────
        self.frames += 1;
        let frame_len_bits = 1.0 + (data_bits + parity_bits) as f64 + stop_bits.bits();
        // Quarter-bit tolerance for start-edge jitter on back-to-back frames.
        self.resume_after = t0 + ((frame_len_bits - 0.25) * bit_ns).round() as u64;

        debug!(
            "[{}] frame #{}: 0x{:02X} at {}ns{}",
            self.name,
            self.frames,
            value,
            t0,
            if frame_error { " (error)" } else { "" }
        );
        if frame_error && let Some(errors) = &error_out {
            errors.send(Trigger::new(t0))?;
        }
        if let Some(bits_out) = bits_out {
            let bit_duration = bit_ns.round() as u64;
            let annotations = bits
                .into_iter()
                .enumerate()
                .map(|(index, bit)| {
                    Word::spanning(
                        u64::from(bit),
                        t0 + ((1.0 + index as f64) * bit_ns).round() as u64,
                        bit_duration,
                    )
                })
                .collect();
            bits_out.send_batch(annotations)?;
        }
        // The word spans its whole frame: start edge through the stop bits.
        if let Some(words_out) = words_out {
            words_out.send(Word::spanning(
                value,
                t0,
                (frame_len_bits * bit_ns).round() as u64,
            ))?;
        }
        if let Some(frame_out) = frame_out {
            let bit_duration = bit_ns.round() as u64;
            let mut annotations = vec![
                Word::spanning(u64::MAX, t0, bit_duration),
                Word::spanning(
                    if frame_error { u64::MAX - 2 } else { value },
                    t0 + bit_duration,
                    bit_duration * data_bits as u64,
                ),
            ];
            if stop_bits.bits() > 0.0 {
                annotations.push(Word::spanning(
                    u64::MAX - 1,
                    t0 + bit_duration * (data_bits as u64 + 1),
                    (stop_bits.bits() * bit_ns).round() as u64,
                ));
            }
            frame_out.send_batch(annotations)?;
        }
        self.finished = rx.is_shutdown();
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::bounded;
    use signal_processing::{ChannelMessage, Sender, Watchdog};

    use super::*;

    const BIT: u64 = 1_000; // 1 Mbaud → 1000 ns/bit

    /// Builds the edge stream for a sequence of frames on an idle-high line.
    /// Each frame: start(0) + data bits (lsb-first) + optional parity + stop(1).
    fn frames_to_edges(
        frames: &[(u64, u16, Option<bool>)], // (t0, value, parity_bit)
        data_bits: usize,
        invert: bool,
    ) -> Vec<Sample> {
        let idle = !invert;
        let mut levels = vec![(idle, 0u64)];
        for &(t0, value, parity_bit) in frames {
            let mut bits = vec![false]; // start
            for i in 0..data_bits {
                bits.push((value >> i) & 1 == 1);
            }
            if let Some(p) = parity_bit {
                bits.push(p);
            }
            bits.push(true); // stop
            for (i, logical_level) in bits.iter().enumerate() {
                levels.push((*logical_level != invert, t0 + i as u64 * BIT));
            }
            // Line returns to idle after the stop bit interval ends.
            levels.push((idle, t0 + bits.len() as u64 * BIT));
        }
        // Collapse to edges (level changes only).
        let mut edges: Vec<Sample> = Vec::new();
        for (level, ts) in levels {
            if edges.last().is_none_or(|last| last.value != level) {
                edges.push(Sample::new(level, ts));
            }
        }
        edges
    }

    struct Output {
        words: Vec<Word>,
        errors: Vec<Trigger>,
    }

    fn run_decoder(decoder: &mut UartDecoder, edges: Vec<Sample>) -> Output {
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<Sample>>(1024);
        for edge in &edges {
            tx.send(ChannelMessage::Sample(*edge)).unwrap();
        }
        drop(tx);
        let inputs = [InputPort::new_with_watchdog(rx, &wd, "uart", "rx")];
        let (words_tx, words_rx) = bounded::<ChannelMessage<Word>>(1024);
        let (err_tx, err_rx) = bounded::<ChannelMessage<Trigger>>(1024);
        let outputs = [
            OutputPort::new_with_watchdog(Sender::new(vec![words_tx]), &wd, "uart", "words"),
            OutputPort::new_with_watchdog(Sender::new(vec![err_tx]), &wd, "uart", "error"),
        ];

        loop {
            match decoder.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        Output {
            words: words_rx
                .try_iter()
                .filter_map(|m| match m {
                    ChannelMessage::Sample(w) => Some(w),
                    _ => None,
                })
                .collect(),
            errors: err_rx
                .try_iter()
                .filter_map(|m| match m {
                    ChannelMessage::Sample(t) => Some(t),
                    _ => None,
                })
                .collect(),
        }
    }

    #[test]
    fn decodes_8n1_frames() {
        let edges = frames_to_edges(&[(10_000, 0x55, None), (50_000, 0xA3, None)], 8, false);
        let mut decoder = UartDecoder::new(1_000_000, 8);
        let out = run_decoder(&mut decoder, edges);
        assert_eq!(
            out.words.iter().map(|w| w.value).collect::<Vec<_>>(),
            vec![0x55, 0xA3]
        );
        assert_eq!(out.words[0].timestamp_ns, 10_000);
        assert_eq!(out.words[1].timestamp_ns, 50_000);
        assert!(out.errors.is_empty());
    }

    #[test]
    fn msb_first_reverses_bits() {
        let edges = frames_to_edges(&[(10_000, 0x01, None)], 8, false);
        let mut decoder = UartDecoder::new(1_000_000, 8).with_bit_order(BitOrder::MsbFirst);
        let out = run_decoder(&mut decoder, edges);
        assert_eq!(out.words[0].value, 0x80);
    }

    #[test]
    fn even_parity_ok_and_error() {
        // 0x03 has two ones — even parity bit is 0.
        let good = frames_to_edges(&[(10_000, 0x03, Some(false))], 8, false);
        let mut decoder = UartDecoder::new(1_000_000, 8).with_parity(UartParity::Even, true);
        let out = run_decoder(&mut decoder, good);
        assert_eq!(out.words[0].value, 0x03);
        assert!(out.errors.is_empty());

        // Wrong parity bit → error trigger, word still emitted.
        let bad = frames_to_edges(&[(10_000, 0x03, Some(true))], 8, false);
        let mut decoder = UartDecoder::new(1_000_000, 8).with_parity(UartParity::Even, true);
        let out = run_decoder(&mut decoder, bad);
        assert_eq!(out.words[0].value, 0x03);
        assert_eq!(out.errors, vec![Trigger::new(10_000)]);
    }

    #[test]
    fn framing_error_on_low_stop_bit() {
        // Build a frame whose "stop bit" is low: value 0xFF then hold the
        // line low across the stop interval by appending a manual edge.
        let mut edges = frames_to_edges(&[(10_000, 0x00, None)], 8, false);
        // frames_to_edges emits stop high at 10_000+9*BIT; overwrite: line
        // stays low through the stop bit, returning high afterwards.
        edges.retain(|e| e.start_time_ns < 10_000 + 9 * BIT);
        edges.push(Sample::new(true, 10_000 + 10 * BIT));
        let mut decoder = UartDecoder::new(1_000_000, 8);
        let out = run_decoder(&mut decoder, edges);
        assert_eq!(out.words[0].value, 0x00);
        assert_eq!(out.errors, vec![Trigger::new(10_000)]);
    }

    #[test]
    fn inverted_line() {
        let edges = frames_to_edges(&[(10_000, 0x5A, None)], 8, true);
        let mut decoder = UartDecoder::new(1_000_000, 8).with_invert(true);
        let out = run_decoder(&mut decoder, edges);
        assert_eq!(out.words[0].value, 0x5A);
        assert!(out.errors.is_empty());
    }

    #[test]
    fn decodes_hello_newline() {
        let text = b"HELLO\n";
        let frames: Vec<(u64, u16, Option<bool>)> = text
            .iter()
            .enumerate()
            .map(|(i, &byte)| (10_000 + i as u64 * 20_000, byte as u16, None))
            .collect();
        let edges = frames_to_edges(&frames, 8, false);
        let mut decoder = UartDecoder::new(1_000_000, 8);
        let out = run_decoder(&mut decoder, edges);
        assert_eq!(
            out.words.iter().map(|w| w.value as u8).collect::<Vec<_>>(),
            text.to_vec()
        );
        assert!(out.errors.is_empty());
    }

    #[test]
    fn decodes_last_frame_when_capture_ends_right_after_stop_bit() {
        // Capture recording stops the instant the last byte's stop bit
        // interval ends — no trailing idle margin, unlike `frames_to_edges`
        // which always appends one.
        let text = b"HELLO\n";
        let frames: Vec<(u64, u16, Option<bool>)> = text
            .iter()
            .enumerate()
            .map(|(i, &byte)| (10_000 + i as u64 * 20_000, byte as u16, None))
            .collect();
        let mut edges = frames_to_edges(&frames, 8, false);
        let last_t0 = frames.last().unwrap().0;
        edges.retain(|e| e.start_time_ns <= last_t0 + 9 * BIT);
        let mut decoder = UartDecoder::new(1_000_000, 8);
        let out = run_decoder(&mut decoder, edges);
        assert_eq!(
            out.words.iter().map(|w| w.value as u8).collect::<Vec<_>>(),
            text.to_vec()
        );
    }

    #[test]
    fn resyncs_after_error() {
        // A framing-error frame followed by a clean frame.
        let mut edges = frames_to_edges(&[(10_000, 0x00, None)], 8, false);
        edges.retain(|e| e.start_time_ns < 10_000 + 9 * BIT);
        edges.push(Sample::new(true, 10_000 + 10 * BIT));
        let clean = frames_to_edges(&[(40_000, 0x42, None)], 8, false);
        // Skip the initial idle sample of the second batch (line already high).
        edges.extend(clean.into_iter().skip(1));

        let mut decoder = UartDecoder::new(1_000_000, 8);
        let out = run_decoder(&mut decoder, edges);
        assert_eq!(
            out.words.iter().map(|w| w.value).collect::<Vec<_>>(),
            vec![0x00, 0x42]
        );
        assert_eq!(out.errors.len(), 1);
    }
}
