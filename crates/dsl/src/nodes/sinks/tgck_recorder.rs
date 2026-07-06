//! TGCK line-boundary recorder (design §7 Phase 6)
//!
//! Restores the per-capture `*_tgck.csv` feature of the original
//! `ControlledParallelWriter`: for every TGCK cycle it records where the
//! line boundary fell in the captured byte stream — byte index and
//! timestamp of the rising and falling edge, plus the first data word
//! (ACDK strobe) after each. Windows are keyed on the `filename` level, so
//! the recorder stays aligned with the `BinaryFileWriter` capturing the
//! same stream: `output/capture_0001.bin` gets
//! `output/capture_0001_tgck.csv`.

use crate::nodes::decoders::ParallelWord;
use crate::runtime::events::TextSample;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};
use crate::runtime::sample::Sample;
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use tracing::{debug, info, warn};

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

#[derive(Default)]
struct Window {
    filename: String,
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

    /// `output/capture_0001.bin` → `output/capture_0001_tgck.csv`.
    fn csv_path(&self) -> PathBuf {
        let path = PathBuf::from(&self.filename);
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "capture".to_string());
        path.with_file_name(format!("{stem}_tgck.csv"))
    }

    fn write_csv(&self) -> std::io::Result<()> {
        if self.records.is_empty() {
            return Ok(());
        }
        let path = self.csv_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(&path)?;
        let mut writer = std::io::BufWriter::new(file);
        writeln!(
            writer,
            "rising_byte_index,rising_timestamp,falling_byte_index,falling_timestamp,first_clock_rising_byte_index,first_clock_rising_timestamp,first_clock_falling_byte_index,first_clock_falling_timestamp"
        )?;
        for record in &self.records {
            writeln!(
                writer,
                "{},{},{},{},{},{},{},{}",
                record.rising_byte_index,
                record.rising_timestamp,
                record.falling_byte_index,
                record.falling_timestamp,
                record.first_word_after_rising_byte_index,
                record.first_word_after_rising_timestamp,
                record.first_word_after_falling_byte_index,
                record.first_word_after_falling_timestamp,
            )?;
        }
        writer.flush()?;
        info!(
            "Wrote TGCK CSV {} with {} records",
            path.display(),
            self.records.len()
        );
        Ok(())
    }
}

/// Sink correlating TGCK line-clock edges with the captured byte stream.
///
/// Inputs: `words` — `ParallelWord` (the enable-gated data stream, same as
/// the writer's); `tgck` — `Sample` edges; `filename` — `TextSample` level
/// (never blocked on, §3.1). A window opens at the first word after a
/// filename change and closes (writing its CSV) at the next change or at
/// end-of-stream; TGCK edges outside an open window are ignored, matching
/// the original writer.
pub struct TgckRecorder {
    name: String,
    window: Option<Window>,
    current_filename: Option<String>,
    pending_names: VecDeque<TextSample>,
    last_tgck: bool,
    tgck_closed: bool,
    filename_closed: bool,
    words_buffer: VecDeque<ParallelWord>,
    tgck_buffer: VecDeque<Sample>,
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
            words_buffer: VecDeque::new(),
            tgck_buffer: VecDeque::new(),
            name_buffer: VecDeque::new(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    fn close_window(&mut self) -> Result<(), WorkError> {
        if let Some(mut window) = self.window.take() {
            window.finalize_record();
            window
                .write_csv()
                .map_err(|e| WorkError::NodeError(format!("TGCK CSV write failed: {e}")))?;
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
        0
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![
            PortSchema::new::<ParallelWord>("words", 0, PortDirection::Input),
            PortSchema::new::<Sample>("tgck", 1, PortDirection::Input),
            PortSchema::new::<TextSample>("filename", 2, PortDirection::Input),
        ]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let word = {
            let mut words = inputs
                .first()
                .and_then(|port| port.get::<ParallelWord>(&mut self.words_buffer))
                .ok_or_else(|| WorkError::NodeError("Missing words input".to_string()))?;
            match words.recv() {
                Ok(word) => word,
                Err(WorkError::Shutdown) => {
                    self.close_window()?;
                    return Err(WorkError::Shutdown);
                }
                Err(e) => return Err(e),
            }
        };
        let position = word.timing.position;

        // Filename changes: never block (§3.1), apply those at or before
        // this word — each one closes the current window.
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
            if next.start_time > position {
                break;
            }
            let sample = self.pending_names.pop_front().expect("peeked");
            debug!("[{}] filename -> '{}'", self.name, sample.value);
            self.close_window()?;
            self.current_filename = Some(sample.value);
        }

        // TGCK edges up to this word's position (blocking horizon, like the
        // original writer: the recorder waits for the line clock to catch
        // up rather than mis-attributing boundaries).
        if !self.tgck_closed {
            let mut tgck = inputs
                .get(1)
                .and_then(|port| port.get::<Sample>(&mut self.tgck_buffer))
                .ok_or_else(|| WorkError::NodeError("Missing tgck input".to_string()))?;
            loop {
                match tgck.peek() {
                    Ok(edge) if edge.start_time <= position => {
                        let edge = tgck.recv()?;
                        if let Some(window) = &mut self.window {
                            if edge.value && !self.last_tgck {
                                window.finalize_record();
                                window.current_rising = Some((window.words, edge.start_time));
                                window.need_after_rising = true;
                            }
                            if !edge.value && self.last_tgck {
                                window.current_falling = Some((window.words, edge.start_time));
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
            let Some(filename) = self.current_filename.clone() else {
                warn!("[{}] word before any filename; not recorded", self.name);
                return Ok(1);
            };
            self.window = Some(Window {
                filename,
                ..Window::default()
            });
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
    use super::*;
    use crate::TimingInfo;
    use crate::runtime::sender::ChannelMessage;
    use crate::runtime::watchdog::Watchdog;
    use crossbeam_channel::bounded;

    fn word(ts: u64) -> ParallelWord {
        ParallelWord {
            value: 0xAB,
            timing: TimingInfo::new(ts as f64 / 1_000.0, ts),
        }
    }

    #[test]
    fn records_line_boundaries_per_window() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("capture_0001.bin");
        let file_b = dir.path().join("capture_0002.bin");

        let wd = Watchdog::new();
        let (words_tx, words_rx) = bounded::<ChannelMessage<ParallelWord>>(64);
        let (tgck_tx, tgck_rx) = bounded::<ChannelMessage<Sample>>(64);
        let (name_tx, name_rx) = bounded::<ChannelMessage<TextSample>>(64);

        // Window A: words at 100..=104; TGCK rising 101, falling 103.
        // Window B (name change at 200): words 200..=201; the rising at 200
        // lands between windows (A closed by the name change, B not yet
        // opened by a word) and is dropped — the original writer behaved
        // the same way for edges before a window's first word.
        for message in [
            ChannelMessage::Sample(TextSample::new(file_a.display().to_string(), 0)),
            ChannelMessage::Sample(TextSample::new(file_b.display().to_string(), 200)),
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

        let inputs = [
            InputPort::new_with_watchdog(words_rx, &wd, "tgck", "words"),
            InputPort::new_with_watchdog(tgck_rx, &wd, "tgck", "tgck"),
            InputPort::new_with_watchdog(name_rx, &wd, "tgck", "filename"),
        ];
        let mut recorder = TgckRecorder::new();
        loop {
            match recorder.work(&inputs, &[]) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        let csv_a = std::fs::read_to_string(dir.path().join("capture_0001_tgck.csv"))
            .expect("window A csv");
        let mut lines = csv_a.lines();
        assert!(lines.next().unwrap().starts_with("rising_byte_index"));
        // Edges ≤ a word's position are drained before that word counts:
        // rising@101 lands at byte index 1 (only word@100 written) and the
        // word@101 is the first word after it; falling@103 at index 3 with
        // word@103 the first after.
        assert_eq!(lines.next().unwrap(), "1,101,3,103,1,101,3,103");
        assert!(lines.next().is_none());

        // Window B saw no in-window TGCK cycle → no CSV.
        assert!(!dir.path().join("capture_0002_tgck.csv").exists());
    }

    #[test]
    fn no_csv_without_records() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("capture_0001.bin");

        let wd = Watchdog::new();
        let (words_tx, words_rx) = bounded::<ChannelMessage<ParallelWord>>(16);
        let (tgck_tx, tgck_rx) = bounded::<ChannelMessage<Sample>>(16);
        let (name_tx, name_rx) = bounded::<ChannelMessage<TextSample>>(16);
        name_tx
            .send(ChannelMessage::Sample(TextSample::new(
                file.display().to_string(),
                0,
            )))
            .unwrap();
        drop(name_tx);
        drop(tgck_tx);
        words_tx.send(ChannelMessage::Sample(word(100))).unwrap();
        drop(words_tx);

        let inputs = [
            InputPort::new_with_watchdog(words_rx, &wd, "tgck", "words"),
            InputPort::new_with_watchdog(tgck_rx, &wd, "tgck", "tgck"),
            InputPort::new_with_watchdog(name_rx, &wd, "tgck", "filename"),
        ];
        let mut recorder = TgckRecorder::new();
        loop {
            match recorder.work(&inputs, &[]) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert!(!dir.path().join("capture_0001_tgck.csv").exists());
    }
}
