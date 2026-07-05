//! Binary file writer — words to files, rolled over by a filename level
//!
//! The writer blocks on its `data` input only — never on `filename`. Per the
//! level-stream contract the filename is always defined (its producer emits
//! the initial value at t=0), so the writer keeps a *current filename*
//! register and applies filename changes as their timestamps are passed by
//! the data stream. Files open lazily on the first word written to them, so
//! name windows without data produce no file at all.

use crate::nodes::decoders::ParallelWord;
use crate::runtime::events::TextSample;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// How a word's value is written to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteWidth {
    /// Low byte only (`value as u8`).
    #[default]
    U8,
    /// Little-endian 16-bit.
    U16Le,
    /// Little-endian 32-bit.
    U32Le,
}

impl WriteWidth {
    fn write_to(&self, writer: &mut impl Write, value: u64) -> std::io::Result<usize> {
        match self {
            WriteWidth::U8 => {
                writer.write_all(&[value as u8])?;
                Ok(1)
            }
            WriteWidth::U16Le => {
                writer.write_all(&(value as u16).to_le_bytes())?;
                Ok(2)
            }
            WriteWidth::U32Le => {
                writer.write_all(&(value as u32).to_le_bytes())?;
                Ok(4)
            }
        }
    }
}

/// Sink writing [`ParallelWord`] values to files named by a
/// [`TextSample`] level.
///
/// Inputs: `data` (0) — `ParallelWord`; `filename` (1) — `TextSample` level
/// Outputs: none
pub struct BinaryFileWriter {
    name: String,
    width: WriteWidth,
    index_csv: bool,

    data_buffer: VecDeque<ParallelWord>,
    name_buffer: VecDeque<TextSample>,
    /// Drained but not yet applicable name changes (timestamps ahead of the
    /// data stream), in channel (= timestamp) order.
    pending_names: VecDeque<TextSample>,

    current_name: Option<String>,
    current_file: Option<BufWriter<File>>,
    files_closed: usize,
    bytes_in_file: u64,
    words_in_file: u64,
    file_start_time_us: f64,
    file_end_time_us: f64,
    file_start_pos: u64,
    file_end_pos: u64,
    last_word_ts: u64,
}

impl BinaryFileWriter {
    pub fn new() -> Self {
        Self {
            name: "binary_file_writer".to_string(),
            width: WriteWidth::default(),
            index_csv: false,
            data_buffer: VecDeque::new(),
            name_buffer: VecDeque::new(),
            pending_names: VecDeque::new(),
            current_name: None,
            current_file: None,
            files_closed: 0,
            bytes_in_file: 0,
            words_in_file: 0,
            file_start_time_us: 0.0,
            file_end_time_us: 0.0,
            file_start_pos: 0,
            file_end_pos: 0,
            last_word_ts: 0,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_width(mut self, width: WriteWidth) -> Self {
        self.width = width;
        self
    }

    /// Append a row per closed file to `captures.csv` next to the data files.
    pub fn with_index_csv(mut self, enabled: bool) -> Self {
        self.index_csv = enabled;
        self
    }

    fn write_index_entry(&self, path: &Path) -> std::io::Result<()> {
        let index_path = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("captures.csv");
        let exists = index_path.exists();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&index_path)?;
        let mut writer = BufWriter::new(file);
        if !exists {
            writeln!(
                writer,
                "file_num,filename,bytes,start_time_us,end_time_us,duration_us,start_pos,end_pos"
            )?;
        }
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        writeln!(
            writer,
            "{},{},{},{:.6},{:.6},{:.6},{},{}",
            self.files_closed,
            filename,
            self.bytes_in_file,
            self.file_start_time_us,
            self.file_end_time_us,
            self.file_end_time_us - self.file_start_time_us,
            self.file_start_pos,
            self.file_end_pos,
        )?;
        writer.flush()
    }

    /// Flush and close the current file, if any.
    fn close_current(&mut self) -> std::io::Result<()> {
        if let Some(mut writer) = self.current_file.take() {
            writer.flush()?;
            self.files_closed += 1;
            let path = PathBuf::from(self.current_name.as_deref().unwrap_or_default());
            info!(
                "[{}] closed {} ({} words, {} bytes)",
                self.name,
                path.display(),
                self.words_in_file,
                self.bytes_in_file
            );
            if self.index_csv {
                self.write_index_entry(&path)?;
            }
        } else if self.words_in_file == 0 && self.current_name.is_some() {
            debug!(
                "[{}] name window {:?} had no data, no file created",
                self.name, self.current_name
            );
        }
        self.bytes_in_file = 0;
        self.words_in_file = 0;
        Ok(())
    }

    /// Switch to a new filename, closing the current file.
    fn apply_name_change(&mut self, change: TextSample) -> WorkResult<()> {
        if change.start_time < self.last_word_ts {
            warn!(
                "[{}] filename change to {:?} at {}ns arrived after data at {}ns — \
                 words may have landed at the previous boundary",
                self.name, change.value, change.start_time, self.last_word_ts
            );
        }
        self.close_current()
            .map_err(|e| WorkError::NodeError(format!("closing file: {e}")))?;
        debug!(
            "[{}] filename -> {:?} at {}ns",
            self.name, change.value, change.start_time
        );
        self.current_name = Some(change.value);
        Ok(())
    }

    fn ensure_file_open(&mut self) -> WorkResult<&mut BufWriter<File>> {
        if self.current_file.is_none() {
            let name = self
                .current_name
                .as_deref()
                .ok_or_else(|| WorkError::NodeError("No filename set".to_string()))?;
            let path = PathBuf::from(name);
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)
                    .map_err(|e| WorkError::NodeError(format!("creating {parent:?}: {e}")))?;
            }
            let file = File::create(&path)
                .map_err(|e| WorkError::NodeError(format!("creating {path:?}: {e}")))?;
            info!("[{}] created {}", self.name, path.display());
            self.current_file = Some(BufWriter::new(file));
        }
        Ok(self.current_file.as_mut().unwrap())
    }
}

impl Default for BinaryFileWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for BinaryFileWriter {
    fn drop(&mut self) {
        if self.current_file.is_some() {
            info!("[{}] shutting down — closing open file", self.name);
            if let Err(e) = self.close_current() {
                warn!("[{}] error closing file on shutdown: {e}", self.name);
            }
        }
    }
}

impl ProcessNode for BinaryFileWriter {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        0
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![
            PortSchema::new::<ParallelWord>("data", 0, PortDirection::Input),
            PortSchema::new::<TextSample>("filename", 1, PortDirection::Input),
        ]
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut data = inputs
            .first()
            .and_then(|port| port.get::<ParallelWord>(&mut self.data_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing data input".to_string()))?;
        let mut names = inputs
            .get(1)
            .and_then(|port| port.get::<TextSample>(&mut self.name_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing filename input".to_string()))?;

        // The initial name is guaranteed by the level-stream contract (sent
        // at t=0), so a blocking wait for it is bounded and only happens once.
        if self.current_name.is_none() && self.pending_names.is_empty() {
            let initial = names.recv()?;
            self.pending_names.push_back(initial);
        }

        // Block for data; on shutdown finalize the open file.
        let word = match data.recv() {
            Ok(word) => word,
            Err(WorkError::Shutdown) => {
                self.close_current()
                    .map_err(|e| WorkError::NodeError(format!("closing file: {e}")))?;
                return Err(WorkError::Shutdown);
            }
            Err(e) => return Err(e),
        };

        // Opportunistically drain name changes (never blocks).
        while let Ok(change) = names.try_recv() {
            self.pending_names.push_back(change);
        }

        // Apply every name change the data stream has passed.
        let word_ts = word.timing.position;
        while self
            .pending_names
            .front()
            .is_some_and(|change| change.start_time <= word_ts)
        {
            let change = self.pending_names.pop_front().unwrap();
            self.apply_name_change(change)?;
        }

        // Append the word to the current file.
        if self.words_in_file == 0 {
            self.file_start_time_us = word.timing.timestamp_us;
            self.file_start_pos = word.timing.position;
        }
        self.file_end_time_us = word.timing.timestamp_us;
        self.file_end_pos = word.timing.position;
        self.last_word_ts = word_ts;

        let width = self.width;
        let value = word.value;
        let writer = self.ensure_file_open()?;
        let bytes = width
            .write_to(writer, value)
            .map_err(|e| WorkError::NodeError(format!("writing word: {e}")))?;
        self.bytes_in_file += bytes as u64;
        self.words_in_file += 1;
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::decoders::TimingInfo;
    use crate::runtime::sender::ChannelMessage;
    use crate::runtime::watchdog::Watchdog;
    use crossbeam_channel::bounded;

    fn word(value: u64, ts: u64) -> ParallelWord {
        ParallelWord {
            value,
            timing: TimingInfo::new(ts as f64 / 1_000.0, ts),
        }
    }

    struct Rig {
        data_tx: crossbeam_channel::Sender<ChannelMessage<ParallelWord>>,
        name_tx: crossbeam_channel::Sender<ChannelMessage<TextSample>>,
        inputs: Vec<InputPort>,
    }

    fn rig() -> Rig {
        let wd = Watchdog::new();
        let (data_tx, data_rx) = bounded::<ChannelMessage<ParallelWord>>(256);
        let (name_tx, name_rx) = bounded::<ChannelMessage<TextSample>>(256);
        Rig {
            data_tx,
            name_tx,
            inputs: vec![
                InputPort::new_with_watchdog(data_rx, &wd, "writer", "data"),
                InputPort::new_with_watchdog(name_rx, &wd, "writer", "filename"),
            ],
        }
    }

    fn run(rig: Rig, writer: &mut BinaryFileWriter) {
        let Rig {
            data_tx,
            name_tx,
            inputs,
        } = rig;
        drop(data_tx);
        drop(name_tx);
        loop {
            match writer.work(&inputs, &[]) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
    }

    #[test]
    fn rolls_files_on_name_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = |n: &str| dir.path().join(n).display().to_string();

        let rig = rig();
        // Initial level at t=0, then a change at 1000.
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(path("a.bin"), 0)))
            .unwrap();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(
                path("b.bin"),
                1_000,
            )))
            .unwrap();
        for (v, ts) in [(1u64, 100u64), (2, 200), (3, 1_000), (4, 1_100)] {
            rig.data_tx
                .send(ChannelMessage::Sample(word(v, ts)))
                .unwrap();
        }

        run(rig, &mut BinaryFileWriter::new());

        assert_eq!(std::fs::read(dir.path().join("a.bin")).unwrap(), vec![1, 2]);
        // The word at exactly the boundary timestamp lands in the new file.
        assert_eq!(std::fs::read(dir.path().join("b.bin")).unwrap(), vec![3, 4]);
    }

    #[test]
    fn empty_name_window_creates_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = |n: &str| dir.path().join(n).display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(path("a.bin"), 0)))
            .unwrap();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(path("b.bin"), 500)))
            .unwrap();
        // All data arrives after the second name — window "a" stays empty.
        rig.data_tx
            .send(ChannelMessage::Sample(word(9, 600)))
            .unwrap();

        run(rig, &mut BinaryFileWriter::new());

        assert!(!dir.path().join("a.bin").exists());
        assert_eq!(std::fs::read(dir.path().join("b.bin")).unwrap(), vec![9]);
    }

    #[test]
    fn shutdown_flushes_open_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("only.bin").display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(&path, 0)))
            .unwrap();
        for i in 0..100u64 {
            rig.data_tx
                .send(ChannelMessage::Sample(word(i, 100 + i)))
                .unwrap();
        }

        run(rig, &mut BinaryFileWriter::new());

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 100);
        assert_eq!(bytes[99], 99);
    }

    #[test]
    fn index_csv_records_closed_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = |n: &str| dir.path().join(n).display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(path("a.bin"), 0)))
            .unwrap();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(
                path("b.bin"),
                1_000,
            )))
            .unwrap();
        rig.data_tx
            .send(ChannelMessage::Sample(word(1, 100)))
            .unwrap();
        rig.data_tx
            .send(ChannelMessage::Sample(word(2, 1_500)))
            .unwrap();

        run(rig, &mut BinaryFileWriter::new().with_index_csv(true));

        let csv = std::fs::read_to_string(dir.path().join("captures.csv")).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 files
        assert!(lines[1].contains("a.bin"));
        assert!(lines[2].contains("b.bin"));
    }

    #[test]
    fn wider_write_widths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.bin").display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(&path, 0)))
            .unwrap();
        rig.data_tx
            .send(ChannelMessage::Sample(word(0xBEEF, 100)))
            .unwrap();

        run(rig, &mut BinaryFileWriter::new().with_width(WriteWidth::U16Le));

        assert_eq!(std::fs::read(&path).unwrap(), vec![0xEF, 0xBE]);
    }
}
