//! TGCK line-boundary recorder.
//!
//! Restores the per-capture `*_tgck.csv` feature of the original
//! `ControlledParallelWriter`: for every TGCK cycle it records where the
//! line boundary fell in the captured byte stream — byte index and
//! timestamp of the rising and falling edge, plus the first data word
//! (ACDK strobe) after each.
//!
//! This node does no file I/O itself — it correlates TGCK edges with the
//! word stream and emits the result as two outputs: `filename` (a
//! `_tgck.csv`-suffixed passthrough of its own `filename` input, so it
//! stays keyed alongside whatever `BinaryFileWriter` is capturing the same
//! stream) and `rows` (each CSV line as a `TextSample` event, header
//! included). Connect both to a [`TextFileWriter`](super::text_file_writer::TextFileWriter)
//! to actually persist them; that split keeps the edge-correlation logic
//! here platform-agnostic (no filesystem needed to compute it) and reuses
//! the same lazy-open-on-first-line file-rolling `TextFileWriter` already
//! provides.

use std::collections::VecDeque;
use std::path::PathBuf;

use tracing::{debug, warn};

use signal_processing::{
    InputPort, OutputPort, PortDirection, PortSchema, ProcessNode, TextSample, Word, WorkError,
    WorkResult,
};

/// One complete TGCK cycle, positioned within the current capture window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgckRecord {
    pub rising_byte_index: usize,
    pub rising_timestamp: u64,
    pub falling_byte_index: usize,
    pub falling_timestamp: u64,
    pub first_word_after_rising_byte_index: usize,
    pub first_word_after_rising_timestamp: u64,
    pub first_word_after_falling_byte_index: usize,
    pub first_word_after_falling_timestamp: u64,
}

impl TgckRecord {
    const CSV_HEADER: &'static str = "rising_byte_index,rising_timestamp,falling_byte_index,falling_timestamp,first_clock_rising_byte_index,first_clock_rising_timestamp,first_clock_falling_byte_index,first_clock_falling_timestamp";

    fn csv_row(&self) -> String {
        format!(
            "{},{},{},{},{},{},{},{}",
            self.rising_byte_index,
            self.rising_timestamp,
            self.falling_byte_index,
            self.falling_timestamp,
            self.first_word_after_rising_byte_index,
            self.first_word_after_rising_timestamp,
            self.first_word_after_falling_byte_index,
            self.first_word_after_falling_timestamp,
        )
    }
}

#[derive(Default)]
struct Window {
    words: usize,
    records: Vec<TgckRecord>,
    current_rising: Option<(usize, u64)>,
    current_falling: Option<(usize, u64)>,
    first_after_rising: Option<(usize, u64)>,
    first_after_falling: Option<(usize, u64)>,
    need_after_rising: bool,
    need_after_falling: bool,
}

impl Window {
    fn finalize_record(&mut self) {
        if let Some((rising_index, rising_ts)) = self.current_rising.take() {
            let (falling_index, falling_ts) = self.current_falling.take().unwrap_or((0, 0));
            let (after_rising_index, after_rising_ts) =
                self.first_after_rising.take().unwrap_or((0, 0));
            let (after_falling_index, after_falling_ts) =
                self.first_after_falling.take().unwrap_or((0, 0));
            self.records.push(TgckRecord {
                rising_byte_index: rising_index,
                rising_timestamp: rising_ts,
                falling_byte_index: falling_index,
                falling_timestamp: falling_ts,
                first_word_after_rising_byte_index: after_rising_index,
                first_word_after_rising_timestamp: after_rising_ts,
                first_word_after_falling_byte_index: after_falling_index,
                first_word_after_falling_timestamp: after_falling_ts,
            });
        }
        self.need_after_rising = false;
        self.need_after_falling = false;
    }

    /// Header + one row per record, ready to send as `TextSample` lines.
    /// `None` if the window never saw a complete cycle (matches the
    /// original writer: name windows without TGCK activity produce no CSV).
    fn csv_lines(&self) -> Option<Vec<String>> {
        if self.records.is_empty() {
            return None;
        }
        let mut lines = Vec::with_capacity(self.records.len() + 1);
        lines.push(TgckRecord::CSV_HEADER.to_string());
        lines.extend(self.records.iter().map(TgckRecord::csv_row));
        Some(lines)
    }
}

/// `output/capture_0001.bin` → `output/capture_0001_tgck.csv`.
fn tgck_csv_path(filename: &str) -> String {
    let path = PathBuf::from(filename);
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "capture".to_string());
    path.with_file_name(format!("{stem}_tgck.csv"))
        .display()
        .to_string()
}

/// Sink correlating TGCK line-clock edges with the captured byte stream.
///
/// Inputs: `words` — `Word` (the enable-gated data stream, same as
/// the writer's); `tgck` — `Sample` edges; `filename` — `TextSample` level
/// (never blocked on, per the level-stream contract). Outputs: `rows` — `TextSample` events, the CSV
/// header and each finalized record; `filename` — `TextSample` level, the
/// `_tgck.csv`-suffixed passthrough of the `filename` input. A window opens
/// at the first word after a filename change and closes (emitting its rows,
/// if any) at the next change or at end-of-stream; TGCK edges outside an
/// open window are ignored, matching the original writer.
pub struct TgckRecorder {
    name: String,
    window: Option<Window>,
    current_filename: Option<String>,
    pending_names: VecDeque<TextSample>,
    last_tgck: bool,
    tgck_closed: bool,
    filename_closed: bool,
    /// Timestamp of the last word processed; used to place the rows emitted
    /// when end-of-stream force-closes the final window.
    last_position: u64,
    words_buffer: VecDeque<Word>,
    tgck_buffer: VecDeque<signal_processing::Sample>,
    name_buffer: VecDeque<TextSample>,
}

impl TgckRecorder {
    pub fn new() -> Self {
        Self {
            name: "tgck_recorder".to_string(),
            window: None,
            current_filename: None,
            pending_names: VecDeque::new(),
            last_tgck: false,
            tgck_closed: false,
            filename_closed: false,
            last_position: 0,
            words_buffer: VecDeque::new(),
            tgck_buffer: VecDeque::new(),
            name_buffer: VecDeque::new(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Closes the current window and, if it produced any records, sends its
    /// CSV lines through `rows`.
    fn close_window(&mut self, outputs: &[OutputPort], at: u64) -> WorkResult<()> {
        let Some(mut window) = self.window.take() else {
            return Ok(());
        };
        window.finalize_record();
        let Some(lines) = window.csv_lines() else {
            debug!("[{}] window closed with no TGCK cycles; no CSV", self.name);
            return Ok(());
        };
        let rows = outputs
            .first()
            .and_then(|port| port.get::<TextSample>())
            .ok_or_else(|| WorkError::NodeError("Missing rows output".to_string()))?;
        for line in lines {
            rows.send(TextSample::new(line, at))?;
        }
        Ok(())
    }
}

impl Default for TgckRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessNode for TgckRecorder {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        3
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![
            PortSchema::new::<Word>("words", 0, PortDirection::Input),
            PortSchema::new::<signal_processing::Sample>("tgck", 1, PortDirection::Input),
            PortSchema::new::<TextSample>("filename", 2, PortDirection::Input),
        ]
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![
            PortSchema::new::<TextSample>("rows", 0, PortDirection::Output),
            PortSchema::new::<TextSample>("filename", 1, PortDirection::Output),
        ]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let word = {
            let mut words = inputs
                .first()
                .and_then(|port| port.get::<Word>(&mut self.words_buffer))
                .ok_or_else(|| WorkError::NodeError("Missing words input".to_string()))?;
            match words.recv() {
                Ok(word) => word,
                Err(WorkError::Shutdown) => {
                    self.close_window(outputs, self.last_position)?;
                    return Err(WorkError::Shutdown);
                }
                Err(e) => return Err(e),
            }
        };
        let position = word.timestamp_ns;
        self.last_position = position;

        // Filename changes: never block (level-stream contract), apply those at or before
        // this word — each one closes the current window and, regardless of
        // whether that window produced rows, passes the derived `_tgck.csv`
        // name through immediately (the level-stream contract requires a
        // downstream TextFileWriter always sees a name before any rows).
        if !self.filename_closed {
            let mut names = inputs
                .get(2)
                .and_then(|port| port.get::<TextSample>(&mut self.name_buffer))
                .ok_or_else(|| WorkError::NodeError("Missing filename input".to_string()))?;
            loop {
                match names.try_recv() {
                    Ok(sample) => self.pending_names.push_back(sample),
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        self.filename_closed = true;
                        break;
                    }
                }
            }
        }
        while let Some(next) = self.pending_names.front() {
            if next.start_time_ns > position {
                break;
            }
            let sample = self.pending_names.pop_front().expect("peeked");
            debug!("[{}] filename -> '{}'", self.name, sample.value);
            self.close_window(outputs, sample.start_time_ns)?;
            let derived = tgck_csv_path(&sample.value);
            let filename_out = outputs
                .get(1)
                .and_then(|port| port.get::<TextSample>())
                .ok_or_else(|| WorkError::NodeError("Missing filename output".to_string()))?;
            filename_out.send(TextSample::new(derived, sample.start_time_ns))?;
            self.current_filename = Some(sample.value);
        }

        // TGCK edges up to this word's position (blocking horizon, like the
        // original writer: the recorder waits for the line clock to catch
        // up rather than mis-attributing boundaries).
        if !self.tgck_closed {
            let mut tgck = inputs
                .get(1)
                .and_then(|port| port.get::<signal_processing::Sample>(&mut self.tgck_buffer))
                .ok_or_else(|| WorkError::NodeError("Missing tgck input".to_string()))?;
            loop {
                match tgck.peek() {
                    Ok(edge) if edge.start_time_ns <= position => {
                        let edge = tgck.recv()?;
                        if let Some(window) = &mut self.window {
                            if edge.value && !self.last_tgck {
                                window.finalize_record();
                                window.current_rising = Some((window.words, edge.start_time_ns));
                                window.need_after_rising = true;
                            }
                            if !edge.value && self.last_tgck {
                                window.current_falling = Some((window.words, edge.start_time_ns));
                                window.need_after_falling = true;
                            }
                        }
                        self.last_tgck = edge.value;
                    }
                    Ok(_) => break,
                    Err(WorkError::Shutdown) => {
                        self.tgck_closed = true;
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        // The word itself: opens the window if needed, satisfies pending
        // first-word-after markers, advances the byte index.
        if self.window.is_none() {
            if self.current_filename.is_none() {
                warn!("[{}] word before any filename; not recorded", self.name);
                return Ok(1);
            }
            self.window = Some(Window::default());
        }
        let window = self.window.as_mut().expect("window opened above");
        if window.need_after_rising {
            window.first_after_rising = Some((window.words, position));
            window.need_after_rising = false;
        }
        if window.need_after_falling {
            window.first_after_falling = Some((window.words, position));
            window.need_after_falling = false;
        }
        window.words += 1;

        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::bounded;
    use signal_processing::{ChannelMessage, Sample, Watchdog};

    use super::*;

    fn word(ts: u64) -> Word {
        Word::new(0xAB, ts)
    }

    struct Rig {
        rows: crossbeam_channel::Receiver<ChannelMessage<TextSample>>,
        filenames: crossbeam_channel::Receiver<ChannelMessage<TextSample>>,
        inputs: Vec<InputPort>,
        outputs: Vec<OutputPort>,
    }

    fn rig() -> Rig {
        let wd = Watchdog::new();
        let (words_tx, words_rx) = bounded::<ChannelMessage<Word>>(64);
        let (tgck_tx, tgck_rx) = bounded::<ChannelMessage<Sample>>(64);
        let (name_tx, name_rx) = bounded::<ChannelMessage<TextSample>>(64);
        let (rows_tx, rows_rx) = bounded::<ChannelMessage<TextSample>>(64);
        let (filename_tx, filename_rx) = bounded::<ChannelMessage<TextSample>>(64);

        for message in [
            ChannelMessage::Sample(TextSample::new("capture_0001.bin", 0)),
            ChannelMessage::Sample(TextSample::new("capture_0002.bin", 200)),
        ] {
            name_tx.send(message).unwrap();
        }
        drop(name_tx);
        for edge in [
            Sample::new(true, 101),
            Sample::new(false, 103),
            Sample::new(true, 200),
            Sample::new(false, 300),
        ] {
            tgck_tx.send(ChannelMessage::Sample(edge)).unwrap();
        }
        drop(tgck_tx);
        for ts in [100u64, 101, 102, 103, 104, 200, 201] {
            words_tx.send(ChannelMessage::Sample(word(ts))).unwrap();
        }
        drop(words_tx);

        Rig {
            rows: rows_rx,
            filenames: filename_rx,
            inputs: vec![
                InputPort::new_with_watchdog(words_rx, &wd, "tgck", "words"),
                InputPort::new_with_watchdog(tgck_rx, &wd, "tgck", "tgck"),
                InputPort::new_with_watchdog(name_rx, &wd, "tgck", "filename"),
            ],
            outputs: vec![
                OutputPort::new_with_watchdog(
                    signal_processing::Sender::new(vec![rows_tx]),
                    &wd,
                    "tgck",
                    "rows",
                ),
                OutputPort::new_with_watchdog(
                    signal_processing::Sender::new(vec![filename_tx]),
                    &wd,
                    "tgck",
                    "filename",
                ),
            ],
        }
    }

    fn drain(rx: &crossbeam_channel::Receiver<ChannelMessage<TextSample>>) -> Vec<String> {
        rx.try_iter()
            .filter_map(|message| match message {
                ChannelMessage::Sample(sample) => Some(sample.value),
                ChannelMessage::Batch(_) => None,
                ChannelMessage::EndOfStream => None,
            })
            .collect()
    }

    #[test]
    fn records_line_boundaries_per_window() {
        let rig = rig();
        let mut recorder = TgckRecorder::new();
        loop {
            match recorder.work(&rig.inputs, &rig.outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        drop(rig.outputs);

        let filenames = drain(&rig.filenames);
        assert_eq!(
            filenames,
            vec!["capture_0001_tgck.csv", "capture_0002_tgck.csv"]
        );

        let rows = drain(&rig.rows);
        assert_eq!(rows.len(), 2, "header + one record: {rows:?}");
        assert!(rows[0].starts_with("rising_byte_index"));
        // Edges ≤ a word's position are drained before that word counts:
        // rising@101 lands at byte index 1 (only word@100 written) and the
        // word@101 is the first word after it; falling@103 at index 3 with
        // word@103 the first after.
        assert_eq!(rows[1], "1,101,3,103,1,101,3,103");
    }

    #[test]
    fn no_rows_without_records() {
        let wd = Watchdog::new();
        let (words_tx, words_rx) = bounded::<ChannelMessage<Word>>(16);
        let (tgck_tx, tgck_rx) = bounded::<ChannelMessage<Sample>>(16);
        let (name_tx, name_rx) = bounded::<ChannelMessage<TextSample>>(16);
        let (rows_tx, rows_rx) = bounded::<ChannelMessage<TextSample>>(16);
        let (filename_tx, filename_rx) = bounded::<ChannelMessage<TextSample>>(16);

        name_tx
            .send(ChannelMessage::Sample(TextSample::new(
                "capture_0001.bin",
                0,
            )))
            .unwrap();
        drop(name_tx);
        drop(tgck_tx);
        words_tx.send(ChannelMessage::Sample(word(100))).unwrap();
        drop(words_tx);

        let inputs = vec![
            InputPort::new_with_watchdog(words_rx, &wd, "tgck", "words"),
            InputPort::new_with_watchdog(tgck_rx, &wd, "tgck", "tgck"),
            InputPort::new_with_watchdog(name_rx, &wd, "tgck", "filename"),
        ];
        let outputs = vec![
            OutputPort::new_with_watchdog(
                signal_processing::Sender::new(vec![rows_tx]),
                &wd,
                "tgck",
                "rows",
            ),
            OutputPort::new_with_watchdog(
                signal_processing::Sender::new(vec![filename_tx]),
                &wd,
                "tgck",
                "filename",
            ),
        ];
        let mut recorder = TgckRecorder::new();
        loop {
            match recorder.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        drop(outputs);

        // The filename still passes through (level contract)...
        assert_eq!(drain(&filename_rx), vec!["capture_0001_tgck.csv"]);
        // ...but no CSV rows were ever produced.
        assert!(drain(&rows_rx).is_empty());
    }
}
