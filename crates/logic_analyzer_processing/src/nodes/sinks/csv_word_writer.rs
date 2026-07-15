//! CSV word writer for decoded words, rolled over by a filename level.
//!
//! Same shape as [`TextFileWriter`](super::text_file_writer::TextFileWriter),
//! specialized for [`Word`] streams: writes one `id,time_ns,value` row per
//! word, with the value rendered as decimal or zero-padded hex. A generic
//! replacement for ad hoc "dump this decoder's output to CSV" sinks (e.g.
//! the old `SpiCsvWriter` in `examples/spi_decode.rs`).
//!
//! The writer blocks on its `data` input only — never on `filename`. Per
//! the level-stream contract the filename is always defined (its producer
//! emits the initial value at t=0), so the writer keeps a *current filename*
//! register and applies filename changes as their timestamps are passed by
//! the data stream. Files open lazily on the first word written to them, so
//! name windows without data produce no file at all.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use tracing::{debug, info, warn};

use signal_processing::errors::{WorkError, WorkResult};
use signal_processing::events::{TextSample, Word};
use signal_processing::node::ProcessNode;
use signal_processing::ports::{InputPort, OutputPort, PortDirection, PortSchema};

/// How a word's value is rendered in the CSV `value` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CsvValueFormat {
    #[default]
    Decimal,
    /// Uppercase hex, zero-padded to `width` digits.
    Hex { width: usize },
}

impl CsvValueFormat {
    fn render(&self, value: u64) -> String {
        match *self {
            CsvValueFormat::Decimal => value.to_string(),
            CsvValueFormat::Hex { width } => format!("{value:0width$X}"),
        }
    }
}

/// Sink writing one CSV row per [`Word`] to files named by a [`TextSample`] level.
///
/// Inputs: `data` (0) — `Word`; `filename` (1) — `TextSample` level,
/// optional when a static path was set via [`Self::with_filename`].
/// Outputs: none.
pub struct CsvWordWriter {
    name: String,
    header: Option<String>,
    value_format: CsvValueFormat,

    data_buffer: VecDeque<Word>,
    name_buffer: VecDeque<TextSample>,
    /// Drained but not yet applicable name changes (timestamps ahead of the
    /// data stream), in channel (= timestamp) order.
    pending_names: VecDeque<TextSample>,

    current_name: Option<String>,
    current_file: Option<BufWriter<File>>,
    rows_in_file: u64,
    last_word_ts: u64,
}

impl CsvWordWriter {
    pub fn new() -> Self {
        Self {
            name: "csv_word_writer".to_string(),
            header: Some("id,time_ns,value".to_string()),
            value_format: CsvValueFormat::default(),
            data_buffer: VecDeque::new(),
            name_buffer: VecDeque::new(),
            pending_names: VecDeque::new(),
            current_name: None,
            current_file: None,
            rows_in_file: 0,
            last_word_ts: 0,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// CSV header row written once per opened file; `None` omits it.
    pub fn with_header(mut self, header: Option<String>) -> Self {
        self.header = header;
        self
    }

    pub fn with_value_format(mut self, format: CsvValueFormat) -> Self {
        self.value_format = format;
        self
    }

    /// Fixed output path, used when no `filename` input is connected. A
    /// connected filename stream takes precedence (its t=0 level replaces
    /// this before the first word is written).
    pub fn with_filename(mut self, path: impl Into<String>) -> Self {
        self.current_name = Some(path.into());
        self
    }

    /// Flush and close the current file, if any.
    fn close_current(&mut self) -> std::io::Result<()> {
        if let Some(mut writer) = self.current_file.take() {
            writer.flush()?;
            info!(
                "[{}] closed {} ({} rows)",
                self.name,
                self.current_name.as_deref().unwrap_or_default(),
                self.rows_in_file
            );
        } else if self.rows_in_file == 0 && self.current_name.is_some() {
            debug!(
                "[{}] name window {:?} had no data, no file created",
                self.name, self.current_name
            );
        }
        self.rows_in_file = 0;
        Ok(())
    }

    /// Switch to a new filename, closing the current file.
    fn apply_name_change(&mut self, change: TextSample) -> WorkResult<()> {
        if change.start_time_ns < self.last_word_ts {
            warn!(
                "[{}] filename change to {:?} at {}ns arrived after data at {}ns — \
                 words may have landed at the previous boundary",
                self.name, change.value, change.start_time_ns, self.last_word_ts
            );
        }
        self.close_current()
            .map_err(|e| WorkError::NodeError(format!("closing file: {e}")))?;
        debug!(
            "[{}] filename -> {:?} at {}ns",
            self.name, change.value, change.start_time_ns
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
            let mut writer = BufWriter::new(file);
            if let Some(header) = &self.header {
                writer
                    .write_all(header.as_bytes())
                    .and_then(|_| writer.write_all(b"\n"))
                    .map_err(|e| WorkError::NodeError(format!("writing header: {e}")))?;
            }
            self.current_file = Some(writer);
        }
        Ok(self.current_file.as_mut().unwrap())
    }

    fn write_word(&mut self, word: Word) -> WorkResult<()> {
        let word_ts = word.timestamp_ns;
        while self
            .pending_names
            .front()
            .is_some_and(|change| change.start_time_ns <= word_ts)
        {
            let change = self.pending_names.pop_front().unwrap();
            self.apply_name_change(change)?;
        }
        self.last_word_ts = word_ts;

        let row_id = self.rows_in_file + 1;
        let value = self.value_format.render(word.value);
        let writer = self.ensure_file_open()?;
        writeln!(writer, "{row_id},{word_ts},{value}")
            .map_err(|e| WorkError::NodeError(format!("writing row: {e}")))?;
        self.rows_in_file += 1;
        Ok(())
    }
}

impl Default for CsvWordWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CsvWordWriter {
    fn drop(&mut self) {
        if self.current_file.is_some() {
            info!("[{}] shutting down — closing open file", self.name);
            if let Err(e) = self.close_current() {
                warn!("[{}] error closing file on shutdown: {e}", self.name);
            }
        }
    }
}

impl ProcessNode for CsvWordWriter {
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
            PortSchema::new::<Word>("data", 0, PortDirection::Input),
            PortSchema::new::<TextSample>("filename", 1, PortDirection::Input),
        ]
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut data = inputs
            .first()
            .and_then(|port| port.get::<Word>(&mut self.data_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing data input".to_string()))?;
        // Optional when a static filename was set via `with_filename` — an
        // unconnected input writes everything to that one path.
        let mut names = inputs
            .get(1)
            .and_then(|port| port.get::<TextSample>(&mut self.name_buffer));
        if names.is_none() && self.current_name.is_none() && self.pending_names.is_empty() {
            return Err(WorkError::NodeError(
                "No filename: connect the filename input or set a static one".to_string(),
            ));
        }

        // The initial name is guaranteed by the level-stream contract (sent
        // at t=0), so a blocking wait for it is bounded and only happens once.
        if let Some(names) = &mut names
            && self.current_name.is_none()
            && self.pending_names.is_empty()
        {
            let initial = names.recv()?;
            self.pending_names.push_back(initial);
        }

        // Block for a word; on shutdown finalize the open file.
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
        if let Some(names) = &mut names {
            while let Ok(change) = names.try_recv() {
                self.pending_names.push_back(change);
            }
        }

        self.write_word(word)?;
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::bounded;
    use signal_processing::sender::ChannelMessage;
    use signal_processing::watchdog::Watchdog;

    use super::*;

    fn word(value: u64, ts: u64) -> Word {
        Word::new(value, ts)
    }

    struct Rig {
        data_tx: crossbeam_channel::Sender<ChannelMessage<Word>>,
        name_tx: crossbeam_channel::Sender<ChannelMessage<TextSample>>,
        inputs: Vec<InputPort>,
    }

    fn rig() -> Rig {
        let wd = Watchdog::new();
        let (data_tx, data_rx) = bounded::<ChannelMessage<Word>>(256);
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

    fn run(rig: Rig, writer: &mut CsvWordWriter) {
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

    /// With a static filename set and the `filename` input unconnected,
    /// everything is written to that one path — the "save dialog on the
    /// node" case, no formatter needed.
    #[test]
    fn static_filename_without_filename_input() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("static.csv").display().to_string();

        let wd = Watchdog::new();
        let (data_tx, data_rx) = bounded::<ChannelMessage<Word>>(256);
        let inputs = vec![
            InputPort::new_with_watchdog(data_rx, &wd, "writer", "data"),
            InputPort::disconnected().with_watchdog(
                wd.clone(),
                "writer".to_string(),
                "filename".to_string(),
            ),
        ];
        for (v, ts) in [(1u64, 100u64), (2, 200), (0x2F, 300)] {
            data_tx.send(ChannelMessage::Sample(word(v, ts))).unwrap();
        }
        drop(data_tx);

        let mut writer = CsvWordWriter::new().with_filename(&target);
        loop {
            match writer.work(&inputs, &[]) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "id,time_ns,value\n1,100,1\n2,200,2\n3,300,47\n"
        );
    }

    #[test]
    fn missing_filename_and_static_is_an_error() {
        let wd = Watchdog::new();
        let (data_tx, data_rx) = bounded::<ChannelMessage<Word>>(4);
        let inputs = vec![
            InputPort::new_with_watchdog(data_rx, &wd, "writer", "data"),
            InputPort::disconnected().with_watchdog(
                wd.clone(),
                "writer".to_string(),
                "filename".to_string(),
            ),
        ];
        data_tx.send(ChannelMessage::Sample(word(1, 100))).unwrap();
        drop(data_tx);

        let mut writer = CsvWordWriter::new();
        assert!(matches!(
            writer.work(&inputs, &[]),
            Err(WorkError::NodeError(_))
        ));
    }

    #[test]
    fn hex_value_format() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("hex.csv").display().to_string();

        let wd = Watchdog::new();
        let (data_tx, data_rx) = bounded::<ChannelMessage<Word>>(4);
        let inputs = vec![
            InputPort::new_with_watchdog(data_rx, &wd, "writer", "data"),
            InputPort::disconnected().with_watchdog(
                wd.clone(),
                "writer".to_string(),
                "filename".to_string(),
            ),
        ];
        data_tx
            .send(ChannelMessage::Sample(word(0x600081, 100)))
            .unwrap();
        drop(data_tx);

        let mut writer = CsvWordWriter::new()
            .with_filename(&target)
            .with_value_format(CsvValueFormat::Hex { width: 6 })
            .with_header(None);
        loop {
            match writer.work(&inputs, &[]) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "1,100,600081\n");
    }

    #[test]
    fn rolls_files_on_name_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = |n: &str| dir.path().join(n).display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(path("a.csv"), 0)))
            .unwrap();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(
                path("b.csv"),
                1_000,
            )))
            .unwrap();
        for (v, ts) in [(1u64, 100u64), (2, 200), (3, 1_000), (4, 1_100)] {
            rig.data_tx
                .send(ChannelMessage::Sample(word(v, ts)))
                .unwrap();
        }

        run(rig, &mut CsvWordWriter::new());

        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.csv")).unwrap(),
            "id,time_ns,value\n1,100,1\n2,200,2\n"
        );
        // The row at exactly the boundary timestamp lands in the new file,
        // with its own id sequence starting over.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.csv")).unwrap(),
            "id,time_ns,value\n1,1000,3\n2,1100,4\n"
        );
    }

    #[test]
    fn empty_name_window_creates_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = |n: &str| dir.path().join(n).display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(path("a.csv"), 0)))
            .unwrap();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(path("b.csv"), 500)))
            .unwrap();
        rig.data_tx
            .send(ChannelMessage::Sample(word(9, 600)))
            .unwrap();

        run(rig, &mut CsvWordWriter::new());

        assert!(!dir.path().join("a.csv").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.csv")).unwrap(),
            "id,time_ns,value\n1,600,9\n"
        );
    }

    #[test]
    fn shutdown_flushes_open_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("only.csv").display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(&path, 0)))
            .unwrap();
        for i in 0..10u64 {
            rig.data_tx
                .send(ChannelMessage::Sample(word(i, 100 + i)))
                .unwrap();
        }

        run(rig, &mut CsvWordWriter::new());

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 11); // header + 10 rows
        assert_eq!(content.lines().last(), Some("10,109,9"));
    }
}
